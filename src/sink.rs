use parking_lot::Mutex;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use destroy_stream;
use play_raw;
use queue;
use source::Done;
use Device;
use Sample;
use Source;

/// Handle to an device that outputs sounds.
///
/// Dropping the `Sink` stops all sounds. You can use `detach` if you want the sounds to continue
/// playing.
pub struct Sink {
    queue_tx: Arc<queue::SourcesQueueInput<f32>>,
    sleep_until_end: Arc<Mutex<Option<Receiver<()>>>>,

    controls: Arc<Controls>,
    sound_count: Arc<AtomicUsize>,

    detached: bool,
}

struct Controls {
    pause: AtomicBool,
    volume: Mutex<f32>,
    stopped: AtomicBool,
}

impl Sink {
    #[inline(always)]
    pub fn new_oneshot<S>(device: &Device, source: S) -> (Self, Receiver<()>)
    where
        S: Source + Send + 'static,
        S::Item: Sample,
        S::Item: Send,
    {
        let (complete_tx, complete_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (queue_tx, queue_rx) = queue::queue_notify_empty(false, done_tx);
        let (engine, stream_id) = play_raw(device, queue_rx);
        let sink = Sink {
            queue_tx: queue_tx,
            sleep_until_end: Arc::new(Mutex::new(None)),
            controls: Arc::new(Controls {
                pause: AtomicBool::new(false),
                volume: Mutex::new(1.0),
                stopped: AtomicBool::new(false),
            }),
            sound_count: Arc::new(AtomicUsize::new(0)),
            detached: false,
        };
        sink.append(source);
        std::thread::spawn(move || {
            if let Some(id) = stream_id {
                let _ = done_rx.recv();
                destroy_stream(&engine, id);
                let _ = complete_tx.send(());
            }
        });
        (sink, complete_rx)
    }

    /// Builds a new `Sink`.
    #[inline]
    pub fn new(device: &Device) -> Sink {
        let (queue_tx, queue_rx) = queue::queue(true);
        play_raw(device, queue_rx);

        Sink {
            queue_tx: queue_tx,
            sleep_until_end: Arc::new(Mutex::new(None)),
            controls: Arc::new(Controls {
                pause: AtomicBool::new(false),
                volume: Mutex::new(1.0),
                stopped: AtomicBool::new(false),
            }),
            sound_count: Arc::new(AtomicUsize::new(0)),
            detached: false,
        }
    }

    /// Appends a sound to the queue of sounds to play.
    #[inline]
    pub fn append<S>(&self, source: S)
    where
        S: Source + Send + 'static,
        S::Item: Sample,
        S::Item: Send,
    {
        let controls = self.controls.clone();

        let source = source
            .pausable(false)
            .amplify(1.0)
            .stoppable()
            .periodic_access(Duration::from_millis(5), move |src| {
                if controls.stopped.load(Ordering::SeqCst) {
                    src.stop();
                } else {
                    src.inner_mut().set_factor(*controls.volume.lock());
                    src.inner_mut()
                        .inner_mut()
                        .set_paused(controls.pause.load(Ordering::SeqCst));
                }
            })
            .convert_samples();
        self.sound_count.fetch_add(1, Ordering::Relaxed);
        let source = Done::new(source, self.sound_count.clone());
        *self.sleep_until_end.lock() = Some(self.queue_tx.append_with_signal(source));
    }

    /// Gets the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than 1.0 will
    /// multiply each sample by this value.
    #[inline]
    pub fn volume(&self) -> f32 {
        *self.controls.volume.lock()
    }

    /// Changes the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than `1.0` will
    /// multiply each sample by this value.
    #[inline]
    pub fn set_volume(&self, value: f32) {
        *self.controls.volume.lock() = value;
    }

    /// Resumes playback of a paused sink.
    ///
    /// No effect if not paused.
    #[inline]
    pub fn play(&self) {
        self.controls.pause.store(false, Ordering::SeqCst);
    }

    /// Pauses playback of this sink.
    ///
    /// No effect if already paused.
    ///
    /// A paused sink can be resumed with `play()`.
    pub fn pause(&self) {
        self.controls.pause.store(true, Ordering::SeqCst);
    }

    /// Gets if a sink is paused
    ///
    /// Sinks can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is paused.
    pub fn is_paused(&self) -> bool {
        self.controls.pause.load(Ordering::SeqCst)
    }

    /// Stops the sink by emptying the queue.
    #[inline]
    pub fn stop(&self) {
        self.controls.stopped.store(true, Ordering::SeqCst);
    }

    /// Destroys the sink without stopping the sounds that are still playing.
    #[inline]
    pub fn detach(mut self) {
        self.detached = true;
    }

    /// Sleeps the current thread until the sound ends.
    #[inline]
    pub fn sleep_until_end(&self) {
        if let Some(sleep_until_end) = self.sleep_until_end.lock().take() {
            let _ = sleep_until_end.recv();
        }
    }

    /// Returns true if this sink has no more sounds to play.
    #[inline]
    pub fn empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of sounds currently in the queue.
    #[inline]
    pub fn len(&self) -> usize {
        self.sound_count.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn listen_end(&self) -> Arc<Mutex<Option<Receiver<()>>>> {
        Arc::clone(&self.sleep_until_end)
    }
}

impl Drop for Sink {
    #[inline]
    fn drop(&mut self) {
        self.queue_tx.set_keep_alive_if_empty(false);

        if !self.detached {
            self.controls.stopped.store(true, Ordering::Relaxed);
        }
    }
}
