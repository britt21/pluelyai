// Pluely macos speaker input and stream
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use anyhow::Result;
use futures_util::Stream;
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};
use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapCons, HeapProd, HeapRb,
};

use cidre::{arc, av, cat, cf, core_audio as ca, ns, os, sc, cm, objc, define_obj_type};
use cidre::sc::stream::{Output, OutputImpl};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
struct MacVersion {
    major: i32,
    minor: i32,
}

impl MacVersion {
    fn current() -> Self {
        eprintln!("[DEBUG] MacVersion::current entry");
        let version = (|| {
            use std::process::Command;
            let output = Command::new("sw_vers")
                .arg("-productVersion")
                .output()
                .ok()?;
            let version_str = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = version_str.trim().split('.').collect();
            if parts.len() >= 2 {
                let major = parts[0].parse().ok()?;
                let minor = parts[1].parse().ok()?;
                Some(Self { major, minor })
            } else {
                None
            }
        })().unwrap_or(Self { major: 13, minor: 0 });
        
        eprintln!("[DEBUG] MacVersion::current version fetched: {}.{}", version.major, version.minor);
        version
    }

    fn supports_ca_tap(&self) -> bool {
        // CATap requires macOS 14.2+
        self.major > 14 || (self.major == 14 && self.minor >= 2)
    }
}

pub enum SpeakerInputInner {
    Tap {
        tap: ca::TapGuard,
        agg_desc: arc::Retained<cf::DictionaryOf<cf::String, cf::Type>>,
    },
    SCStream {
        filter: arc::Retained<sc::ContentFilter>,
        config: arc::Retained<sc::StreamCfg>,
    },
}

pub struct SpeakerInput {
    inner: SpeakerInputInner,
}

struct WakerState {
    waker: Option<Waker>,
    has_data: bool,
}

pub struct SpeakerStream {
    consumer: HeapCons<f32>,
    inner: SpeakerStreamInner,
    ctx: Arc<Ctx>,
    waker_state: Arc<Mutex<WakerState>>,
    current_sample_rate: Arc<AtomicU32>,
}

pub enum SpeakerStreamInner {
    Tap {
        _device: ca::hardware::StartedDevice<ca::AggregateDevice>,
        _tap: ca::TapGuard,
    },
    SCStream {
        _stream: arc::Retained<sc::Stream>,
        _output: arc::Retained<SpeakerAudioOutput>,
        _queue: arc::Retained<cidre::dispatch::Queue>,
    },
}

unsafe impl Send for SpeakerStream {}
unsafe impl Sync for SpeakerStream {}

unsafe impl Send for SpeakerInput {}
unsafe impl Sync for SpeakerInput {}

unsafe impl Send for SpeakerInputInner {}
unsafe impl Sync for SpeakerInputInner {}

struct Ctx {
    format: arc::R<av::AudioFormat>,
    producer: Mutex<HeapProd<f32>>,
    waker_state: Arc<Mutex<WakerState>>,
    current_sample_rate: Arc<AtomicU32>,
    consecutive_drops: Arc<AtomicU32>,
    should_terminate: Arc<AtomicBool>,
}
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

define_obj_type!(
    pub SpeakerAudioOutput + OutputImpl,
    Arc<Ctx>,
    PluelySpeakerAudioOutput
);

unsafe impl Send for SpeakerAudioOutput {}
unsafe impl Sync for SpeakerAudioOutput {}

impl Output for SpeakerAudioOutput {}

#[objc::add_methods]
impl OutputImpl for SpeakerAudioOutput {
    extern "C" fn impl_stream_did_output_sample_buf(
        &mut self,
        _cmd: Option<&ns::Sel>,
        _stream: &sc::Stream,
        sample_buf: &mut cm::SampleBuf,
        kind: sc::OutputType,
    ) {
        if kind == sc::OutputType::Audio {
             static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
             let count = CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
             if count % 100 == 0 {
                 eprintln!("[DEBUG] Audio callback triggered, count: {}", count);
             }

             let ctx = self.inner();
             if let Ok(audio_buf_list) = sample_buf.audio_buf_list::<1>() {
                 if let Some(view) = av::AudioPcmBuf::with_buf_list_no_copy(&ctx.format, audio_buf_list.list(), None) {
                     if let Some(data) = view.data_f32_at(0) {
                          process_audio_data(ctx, &data);
                     }
                 }
            }
        }
    }
}

impl SpeakerStream {
    pub fn sample_rate(&self) -> u32 {
        self.current_sample_rate.load(Ordering::Acquire)
    }
}



impl SpeakerInput {
    pub async fn new(_device_id: Option<String>) -> Result<Self> {
        eprintln!("[DEBUG] macos::SpeakerInput::new entry");
        let version = MacVersion::current();
        eprintln!("[DEBUG] Detected macOS version: {:?}", version);
        
        if version.supports_ca_tap() {
            eprintln!("[DEBUG] Using CATap for audio capture");
            Self::new_tap().await
        } else {
            eprintln!("[DEBUG] Using SCStream fallback for audio capture");
            Self::new_sc_stream().await
        }
    }

    async fn new_tap() -> Result<Self> {
        eprintln!("[DEBUG] Setting up CATap configuration...");
        let output_device = ca::System::default_output_device()?;
        let output_uid = output_device.uid()?;

        let sub_device = cf::DictionaryOf::with_keys_values(
            &[ca::sub_device_keys::uid()],
            &[output_uid.as_type_ref()],
        );

        let tap_desc = ca::TapDesc::with_mono_global_tap_excluding_processes(&ns::Array::new());
        let tap = tap_desc.create_process_tap()?;

        let tap_uid = tap.uid().map_err(|e| anyhow::anyhow!("Failed to get tap UID: {}", e))?;
        let sub_tap = cf::DictionaryOf::with_keys_values(
            &[ca::sub_device_keys::uid()],
            &[tap_uid.as_type_ref()],
        );

        let agg_desc = cf::DictionaryOf::with_keys_values(
            &[
                ca::aggregate_device_keys::is_private(),
                ca::aggregate_device_keys::is_stacked(),
                ca::aggregate_device_keys::tap_auto_start(),
                ca::aggregate_device_keys::name(),
                ca::aggregate_device_keys::main_sub_device(),
                ca::aggregate_device_keys::uid(),
                ca::aggregate_device_keys::sub_device_list(),
                ca::aggregate_device_keys::tap_list(),
            ],
            &[
                cf::Boolean::value_true().as_type_ref(),
                cf::Boolean::value_false(),
                cf::Boolean::value_true(),
                cf::str!(c"system-audio-tap"),
                &output_uid,
                &cf::Uuid::new().to_cf_string(),
                &cf::ArrayOf::from_slice(&[sub_device.as_ref()]),
                &cf::ArrayOf::from_slice(&[sub_tap.as_ref()]),
            ],
        );

        Ok(Self { inner: SpeakerInputInner::Tap { tap, agg_desc } })
    }

    async fn new_sc_stream() -> Result<Self> {
        eprintln!("[DEBUG] Setting up SCStream fallback...");
        
        let (tx, rx) = oneshot::channel();
        let mut tx = Some(tx);
        sc::ShareableContent::current_with_ch(move |content, err| {
            if let Some(tx) = tx.take() {
                let _ = tx.send((content.map(|c| c.retained()), err.map(|e| e.retained())));
            }
        });
        
        let (content, err) = timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| anyhow::anyhow!("Timeout getting shareable content"))?
            .map_err(|_| anyhow::anyhow!("Channel closed"))?;
            
        if let Some(err) = err {
             return Err(anyhow::anyhow!("Failed to get shareable content: {:?}", err));
        }
        
        let content = content.ok_or_else(|| anyhow::anyhow!("No shareable content found"))?;
        let displays = content.displays();
        let display = displays.first().ok_or_else(|| anyhow::anyhow!("No display found"))?.retained();
        
        let filter = sc::ContentFilter::with_display_excluding_windows(&display, &ns::Array::new());
        let mut config = sc::StreamCfg::new();
        config.set_captures_audio(true);
        config.set_sample_rate(48000);
        config.set_channel_count(1);
        config.set_excludes_current_process_audio(true);
        config.set_width(1280);
        config.set_height(720);
        config.set_minimum_frame_interval(cm::Time::with_secs(1.0 / 60.0, 600));
        
        Ok(Self { inner: SpeakerInputInner::SCStream { filter, config: config.retained() } })
    }

    fn start_tap_device_shared(
        agg_desc: &cf::DictionaryOf<cf::String, cf::Type>,
        ctx: &Arc<Ctx>,
    ) -> Result<ca::hardware::StartedDevice<ca::AggregateDevice>> {
        extern "C" fn proc(
            device: ca::Device,
            _now: &cat::AudioTimeStamp,
            input_data: &cat::AudioBufList<1>,
            _input_time: &cat::AudioTimeStamp,
            _output_data: &mut cat::AudioBufList<1>,
            _output_time: &cat::AudioTimeStamp,
            ctx: Option<&mut Ctx>,
        ) -> os::Status {
            let ctx = match ctx {
                Some(c) => c,
                None => return os::Status::NO_ERR,
            };

            ctx.current_sample_rate.store(
                device
                    .actual_sample_rate()
                    .unwrap_or(ctx.format.absd().sample_rate) as u32,
                Ordering::Release,
            );

            if let Some(view) =
                av::AudioPcmBuf::with_buf_list_no_copy(&ctx.format, input_data, None)
            {
                if let Some(data) = view.data_f32_at(0) {
                    process_audio_data(ctx, data);
                }
            } else if ctx.format.common_format() == av::audio::CommonFormat::PcmF32 {
                let first_buffer = &input_data.buffers[0];
                let byte_count = first_buffer.data_bytes_size as usize;
                let float_count = byte_count / std::mem::size_of::<f32>();

                if float_count > 0 && !first_buffer.data.is_null() {
                    let data = unsafe {
                        std::slice::from_raw_parts(first_buffer.data as *const f32, float_count)
                    };
                    process_audio_data(ctx, data);
                }
            }

            os::Status::NO_ERR
        }

        let agg_device = ca::AggregateDevice::with_desc(agg_desc)?;
        let ctx_ptr = Arc::as_ptr(ctx);
        let proc_id = agg_device.create_io_proc_id(proc, Some(unsafe { &mut *(ctx_ptr as *mut Ctx) }))?;
        let started_device = ca::device_start(agg_device, Some(proc_id))?;

        Ok(started_device)
    }

    pub async fn stream(self) -> Result<SpeakerStream> {
        let buffer_size = 1024 * 128;
        let rb = HeapRb::<f32>::new(buffer_size);
        let (producer, consumer) = rb.split();

        let waker_state = Arc::new(Mutex::new(WakerState {
            waker: None,
            has_data: false,
        }));
        
        let should_terminate = Arc::new(AtomicBool::new(false));
        let consecutive_drops = Arc::new(AtomicU32::new(0));

        match self.inner {
            SpeakerInputInner::Tap { tap, agg_desc } => {
                let asbd = tap.asbd().map_err(|e| anyhow::anyhow!("Failed to get tap ASBD: {}", e))?;
                let format = av::AudioFormat::with_asbd(&asbd).ok_or_else(|| anyhow::anyhow!("Failed to create audio format"))?;
                let current_sample_rate = Arc::new(AtomicU32::new(asbd.sample_rate as u32));

                let ctx = Arc::new(Ctx {
                    format,
                    producer: Mutex::new(producer),
                    waker_state: waker_state.clone(),
                    current_sample_rate: current_sample_rate.clone(),
                    consecutive_drops,
                    should_terminate: should_terminate.clone(),
                });

                let device = Self::start_tap_device_shared(&agg_desc, &ctx)?;

                Ok(SpeakerStream {
                    consumer,
                    inner: SpeakerStreamInner::Tap { _device: device, _tap: tap },
                    ctx,
                    waker_state,
                    current_sample_rate,
                })
            }
            SpeakerInputInner::SCStream { filter, config } => {
                let current_sample_rate = Arc::new(AtomicU32::new(48000));
                
                let format = av::AudioFormat::with_common_format_sample_rate_channels_interleaved(
                    av::audio::CommonFormat::PcmF32,
                    48000.0,
                    1,
                    false,
                ).ok_or_else(|| anyhow::anyhow!("Failed to create audio format"))?;

                let ctx = Arc::new(Ctx {
                    format,
                    producer: Mutex::new(producer),
                    waker_state: waker_state.clone(),
                    current_sample_rate: current_sample_rate.clone(),
                    consecutive_drops,
                    should_terminate: should_terminate.clone(),
                });

                let output = SpeakerAudioOutput::with(ctx.clone());
                let queue = cidre::dispatch::Queue::serial_with_ar_pool();
                
                let stream = sc::Stream::new(&filter, &config);
                eprintln!("[DEBUG] SCStream object created");
                stream.add_stream_output(output.as_ref(), sc::OutputType::Audio, Some(&queue)).map_err(|e| anyhow::anyhow!("Failed to add stream output: {:?}", e))?;
                eprintln!("[DEBUG] SCStream output added");
                
                let (tx, rx) = oneshot::channel();
                let mut tx = Some(tx);
                let stream_to_start = stream.retained();
                eprintln!("[DEBUG] Starting SCStream...");
                stream_to_start.start_with_ch(move |err| {
                    eprintln!("[DEBUG] SCStream start completion called, error: {:?}", err);
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(err.map(|e| e.retained()));
                    }
                });
                
                let start_err = timeout(Duration::from_secs(5), rx)
                    .await
                    .map_err(|_| anyhow::anyhow!("Timeout starting SCStream"))?
                    .map_err(|_| anyhow::anyhow!("Channel closed"))?;

                if let Some(err) = start_err {
                    eprintln!("[DEBUG] SCStream start failed: {:?}", err);
                    return Err(anyhow::anyhow!("Failed to start SCStream: {:?}", err));
                }
                eprintln!("[DEBUG] SCStream started successfully");

                Ok(SpeakerStream {
                    consumer,
                    inner: SpeakerStreamInner::SCStream { 
                        _stream: stream,
                        _output: output,
                        _queue: queue,
                    },
                    ctx,
                    waker_state,
                    current_sample_rate,
                })
            }
        }
    }
}


fn process_audio_data(ctx: &Ctx, data: &[f32]) {
    let mut current_peak = 0.0;
    for &sample in data {
        let abs = sample.abs();
        if abs > current_peak {
            current_peak = abs;
        }
    }
    
    // Debug log periodically without unsafe static mut
    static LAST_LOG: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let last = LAST_LOG.load(Ordering::Relaxed);
    if now > last {
        if LAST_LOG.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            eprintln!("[DEBUG] Audio level - Peak: {:.4}, Buffer size: {}", current_peak, data.len());
        }
    }

    if let Ok(mut producer) = ctx.producer.lock() {
        let pushed = producer.push_slice(data);
        
        if pushed < data.len() {
            let consecutive = ctx.consecutive_drops.fetch_add(1, Ordering::AcqRel) + 1;
            if consecutive > 50 {
                ctx.should_terminate.store(true, Ordering::Release);
                return;
            }
        } else {
            ctx.consecutive_drops.store(0, Ordering::Release);
        }
    }

    if let Ok(mut waker_state) = ctx.waker_state.lock() {
        if !waker_state.has_data {
            waker_state.has_data = true;
            if let Some(waker) = waker_state.waker.take() {
                waker.wake();
            }
        }
    }
}

impl Stream for SpeakerStream {
    type Item = f32;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if let Some(sample) = self.consumer.try_pop() {
            return Poll::Ready(Some(sample));
        }

        if self.ctx.should_terminate.load(Ordering::Acquire) {
            return match self.consumer.try_pop() {
                Some(sample) => Poll::Ready(Some(sample)),
                None => Poll::Ready(None),
            };
        }

        if let Ok(mut state) = self.waker_state.lock() {
            state.has_data = false;
            state.waker = Some(cx.waker().clone());
        }

        Poll::Pending
    }
}

impl Drop for SpeakerStream {
    fn drop(&mut self) {
        eprintln!("[DEBUG] SpeakerStream dropping, stopping capture...");
        self.ctx.should_terminate.store(true, Ordering::Release);
        if let SpeakerStreamInner::SCStream { _stream, .. } = &self.inner {
            let (tx, rx) = std::sync::mpsc::channel();
            _stream.stop_with_ch(move |err| {
                tx.send(err.map(|e| e.retained())).ok();
            });
            // Wait for stream to actually stop before dropping Ctx
            let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
        }
    }
}
