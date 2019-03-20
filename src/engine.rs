use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::Builder;

use cpal::Device;
use cpal::EventLoop;
use cpal::Sample as CpalSample;
use cpal::StreamData;
use cpal::StreamId;
use cpal::UnknownTypeOutputBuffer;
use dynamic_mixer;
use source::Source;

/// Plays a source with a device until it ends.
///
/// The playing uses a background thread.
pub fn play_raw<S>(device: &Device, source: S) -> (Arc<Engine>, Option<StreamId>)
where
    S: Source<Item = f32> + Send + 'static,
{
    lazy_static! {
        static ref ENGINE: Arc<Engine> = {
            let engine = Arc::new(Engine {
                events_loop: EventLoop::new(),
                dynamic_mixers: Mutex::new(HashMap::with_capacity(1))
            });

            // We ignore errors when creating the background thread.
            // The user won't get any audio, but that's better than a panic.
            Builder::new()
                .name("rodio audio processing".to_string())
                .spawn({
                    let engine = engine.clone();
                    move || {
                        engine.events_loop.run(|stream_id, buffer| {
                            audio_callback(&engine, stream_id, buffer);
                        })
                    }
                })
                .map_err(|e| println!("Something terrible happened: {}", e))
                .ok()
                .map(|jg| jg.thread().clone());

            engine
        };
    }

    let stream_id = start(&ENGINE, device, source);
    (ENGINE.clone(), stream_id)
}

/// Release the resources and clear the event loop once playing is complete.
pub fn destroy_stream(engine: &Arc<Engine>, id: StreamId) {
    engine.events_loop.destroy_stream(id)
}

// The internal engine of this library.
//
// Each `Engine` owns a thread that runs in the background and plays the audio.
pub struct Engine {
    // The events loop which the streams are created with.
    events_loop: EventLoop,

    dynamic_mixers: Mutex<HashMap<StreamId, dynamic_mixer::DynamicMixer<f32>>>,
}

fn audio_callback(engine: &Arc<Engine>, stream_id: StreamId, buffer: StreamData) {
    let mut dynamic_mixers = engine.dynamic_mixers.lock();

    let mixer_rx = match dynamic_mixers.get_mut(&stream_id) {
        Some(m) => m,
        None => return,
    };

    match buffer {
        StreamData::Output {
            buffer: UnknownTypeOutputBuffer::U16(mut buffer),
        } => {
            for d in buffer.iter_mut() {
                *d = mixer_rx
                    .next()
                    .map(|s| s.to_u16())
                    .unwrap_or(u16::max_value() / 2);
            }
        }
        StreamData::Output {
            buffer: UnknownTypeOutputBuffer::I16(mut buffer),
        } => {
            for d in buffer.iter_mut() {
                *d = mixer_rx.next().map(|s| s.to_i16()).unwrap_or(0i16);
            }
        }
        StreamData::Output {
            buffer: UnknownTypeOutputBuffer::F32(mut buffer),
        } => {
            for d in buffer.iter_mut() {
                *d = mixer_rx.next().unwrap_or(0f32);
            }
        }
        StreamData::Input { buffer: _ } => {
            panic!("Can't play an input stream!");
        }
    };
}

// Builds a new sink that targets a given device.
fn start<S>(engine: &Arc<Engine>, device: &Device, source: S) -> Option<StreamId>
where
    S: Source<Item = f32> + Send + 'static,
{
    let (mixer, stream) = new_output_stream(engine, device);
    engine.events_loop.play_stream(stream.clone());
    mixer.add(source);
    Some(stream)
}

// Adds a new stream to the engine.
// TODO: handle possible errors here
fn new_output_stream(
    engine: &Arc<Engine>,
    device: &Device,
) -> (Arc<dynamic_mixer::DynamicMixerController<f32>>, StreamId) {
    // Determine the format to use for the new stream.
    let format = device
        .default_output_format()
        .expect("The device doesn't support any format!?");

    let stream_id = engine
        .events_loop
        .build_output_stream(device, &format)
        .unwrap();
    let (mixer_tx, mixer_rx) =
        { dynamic_mixer::mixer::<f32>(format.channels, format.sample_rate.0) };

    engine
        .dynamic_mixers
        .lock()
        .insert(stream_id.clone(), mixer_rx);

    (mixer_tx, stream_id)
}
