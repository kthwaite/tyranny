//! DefaultOutput converts our client format; the device's format is never set.

use super::{
    BACKEND_FAILURE, BlockConsumer, INVALID_DEVICE_BUFFER, SAMPLE_RATE, Shared, TIMESTAMP_OVERFLOW,
    frame_duration,
};
use anyhow::{Context, Result, bail, ensure};
use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioOutputUnitStart,
    AudioOutputUnitStop, AudioUnit, AudioUnitAddPropertyListener, AudioUnitGetProperty,
    AudioUnitInitialize, AudioUnitRemovePropertyListenerWithUserData, AudioUnitRenderActionFlags,
    AudioUnitSetProperty, AudioUnitUninitialize, kAudioOutputUnitProperty_CurrentDevice,
    kAudioOutputUnitProperty_IsRunning, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_LastRenderError, kAudioUnitProperty_Latency,
    kAudioUnitProperty_SetRenderCallback, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global,
    kAudioUnitScope_Input, kAudioUnitSubType_DefaultOutput, kAudioUnitType_Output,
};
use objc2_core_audio::{
    AudioConvertHostTimeToNanos, AudioGetCurrentHostTime, AudioObjectGetPropertyData,
    AudioObjectPropertyAddress, kAudioDevicePropertyBufferFrameSize,
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyLatency,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertySafetyOffset,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeOutput,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags,
    kAudio_ParamError, kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM,
};
use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;
use std::mem::{MaybeUninit, align_of, size_of};
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

pub(super) struct CoreAudioOutput {
    unit: AudioUnit,
    callback: Option<Box<CallbackState>>,
    error_listener: bool,
    failed: Cell<bool>,
}

// The owner may move between threads, but cannot be shared (Cell/UnsafeCell).
// CoreAudio serializes rendering for this unit; only that callback accesses
// render. The owner's access to callback-visible state is exclusively atomic.
unsafe impl Send for CoreAudioOutput {}

struct CallbackState {
    render: UnsafeCell<RenderState>,
    shared: Arc<Shared>,
    latency_nanos: AtomicU64,
    render_failed: AtomicBool,
}

struct RenderState {
    consumer: BlockConsumer,
    last_sample_end: Option<u64>,
    last_playback_end: u64,
}

pub(super) fn open(consumer: BlockConsumer) -> Result<CoreAudioOutput> {
    let description = AudioComponentDescription {
        componentType: kAudioUnitType_Output,
        componentSubType: kAudioUnitSubType_DefaultOutput,
        componentManufacturer: kAudioUnitManufacturer_Apple,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    // SAFETY: description lives through the call; a null predecessor asks for
    // the first matching component, which is checked before instantiation.
    let component = unsafe { AudioComponentFindNext(ptr::null_mut(), NonNull::from(&description)) };
    ensure!(
        !component.is_null(),
        "CoreAudio DefaultOutput component is unavailable"
    );
    let mut unit = ptr::null_mut();
    // SAFETY: component is valid and unit is writable pointer-sized storage.
    status(
        unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut unit)) },
        "creating CoreAudio DefaultOutput",
    )?;
    ensure!(
        !unit.is_null(),
        "CoreAudio created a null DefaultOutput instance"
    );

    // Install the RAII owner before any operation that can register a callback
    // or fail. The Box's address remains stable even if the owner is moved.
    let shared = Arc::clone(&consumer.shared);
    let mut output = CoreAudioOutput {
        unit,
        callback: Some(Box::new(CallbackState {
            render: UnsafeCell::new(RenderState {
                consumer,
                last_sample_end: None,
                last_playback_end: 0,
            }),
            shared,
            latency_nanos: AtomicU64::new(0),
            render_failed: AtomicBool::new(false),
        })),
        error_listener: false,
        failed: Cell::new(false),
    };
    let format = AudioStreamBasicDescription {
        mSampleRate: f64::from(SAMPLE_RATE),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: 8,
        mFramesPerPacket: 1,
        mBytesPerFrame: 8,
        mChannelsPerFrame: 2,
        mBitsPerChannel: 32,
        mReserved: 0,
    };
    // This is the CLIENT input of output bus 0, not a device stream/clock
    // property. DefaultOutput owns the conversion to its native output format.
    // SAFETY: unit is live and format has the property's documented ASBD type.
    status(
        unsafe {
            AudioUnitSetProperty(
                unit,
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Input,
                0,
                ptr::from_ref(&format).cast(),
                size_of::<AudioStreamBasicDescription>() as u32,
            )
        },
        "setting CoreAudio client stereo f32/48 kHz format",
    )?;
    let context = ptr::from_ref(output.callback.as_deref().expect("callback is installed"))
        .cast_mut()
        .cast::<c_void>();
    let callback = AURenderCallbackStruct {
        inputProc: Some(render),
        inputProcRefCon: context,
    };
    // SAFETY: callback is copied by the unit; context remains alive until after
    // unit disposal, including every initialization/start failure path.
    status(
        unsafe {
            AudioUnitSetProperty(
                unit,
                kAudioUnitProperty_SetRenderCallback,
                kAudioUnitScope_Input,
                0,
                ptr::from_ref(&callback).cast(),
                size_of::<AURenderCallbackStruct>() as u32,
            )
        },
        "installing CoreAudio render callback",
    )?;
    // SAFETY: the listener shares the same retained context and uses atomics
    // only, including when CoreAudio notifies from its real-time render thread.
    status(
        unsafe {
            AudioUnitAddPropertyListener(
                unit,
                kAudioUnitProperty_LastRenderError,
                Some(render_error),
                context,
            )
        },
        "installing CoreAudio render error listener",
    )?;
    output.error_listener = true;
    // SAFETY: unit has a valid client format and render callback.
    status(
        unsafe { AudioUnitInitialize(unit) },
        "initializing CoreAudio DefaultOutput",
    )?;
    output.refresh_latency()?;
    // Keep rendering silence while logically paused: this reclaims invalidated
    // blocks and observes the final predicted playback deadline.
    // SAFETY: unit is initialized and all callback storage is installed.
    status(
        unsafe { AudioOutputUnitStart(unit) },
        "starting CoreAudio DefaultOutput",
    )?;
    Ok(output)
}

impl CoreAudioOutput {
    pub(super) fn check_error(&self) -> Result<()> {
        if self.failed.get() {
            bail!("CoreAudio output previously failed");
        }
        let result = self.check_native();
        if result.is_err() {
            self.failed.set(true);
            self.state()
                .shared
                .faults
                .fetch_or(BACKEND_FAILURE, Ordering::Release);
        }
        result
    }

    fn state(&self) -> &CallbackState {
        self.callback
            .as_deref()
            .expect("callback is retained until disposal")
    }

    fn check_native(&self) -> Result<()> {
        // SAFETY: these global properties have OSStatus and UInt32 values,
        // respectively, and the unit remains alive throughout the query.
        let error: i32 = unsafe { unit_property(self.unit, kAudioUnitProperty_LastRenderError) }?;
        status(error, "rendering CoreAudio output")?;
        ensure!(
            !self.state().render_failed.load(Ordering::Acquire),
            "CoreAudio reported an asynchronous render failure"
        );
        let running: u32 = unsafe { unit_property(self.unit, kAudioOutputUnitProperty_IsRunning) }?;
        ensure!(running != 0, "CoreAudio DefaultOutput stopped unexpectedly");
        self.refresh_latency()
    }

    fn refresh_latency(&self) -> Result<()> {
        // These read-only queries stay on the owner thread. DefaultOutput can
        // reroute or change native rates without changing our 48 kHz client
        // format; refreshing here avoids a stale startup-only drain estimate.
        // SAFETY: each property is requested with its documented scalar type.
        let (buffer, safety, device_latency, native_rate, unit_latency) = unsafe {
            let device: u32 = unit_property(self.unit, kAudioOutputUnitProperty_CurrentDevice)?;
            ensure!(device != 0, "CoreAudio has no default output device");
            let alive: u32 = device_property(
                device,
                kAudioDevicePropertyDeviceIsAlive,
                kAudioObjectPropertyScopeGlobal,
            )?;
            ensure!(alive != 0, "CoreAudio default output device is unavailable");
            (
                device_property::<u32>(
                    device,
                    kAudioDevicePropertyBufferFrameSize,
                    kAudioObjectPropertyScopeGlobal,
                )?,
                device_property::<u32>(
                    device,
                    kAudioDevicePropertySafetyOffset,
                    kAudioObjectPropertyScopeOutput,
                )?,
                device_property::<u32>(
                    device,
                    kAudioDevicePropertyLatency,
                    kAudioObjectPropertyScopeOutput,
                )?,
                device_property::<f64>(
                    device,
                    kAudioDevicePropertyNominalSampleRate,
                    kAudioObjectPropertyScopeGlobal,
                )?,
                unit_property::<f64>(self.unit, kAudioUnitProperty_Latency)?,
            )
        };
        ensure!(
            native_rate.is_finite() && native_rate > 0.0,
            "CoreAudio returned an invalid native sample rate"
        );
        ensure!(
            unit_latency.is_finite() && unit_latency >= 0.0,
            "CoreAudio returned an invalid processing latency"
        );
        // HAL frame counts belong to the DEVICE clock, not SAMPLE_RATE. The
        // unit's processing delay includes its native format converter.
        let device_frames = u64::from(buffer) + u64::from(safety) + u64::from(device_latency);
        let latency =
            Duration::try_from_secs_f64(device_frames as f64 / native_rate + unit_latency)
                .context("CoreAudio predicted output latency is out of range")?;
        let nanos = u64::try_from(latency.as_nanos())
            .context("CoreAudio predicted output latency overflows its host clock")?;
        self.state().latency_nanos.store(nanos, Ordering::Release);
        Ok(())
    }
}

impl Drop for CoreAudioOutput {
    fn drop(&mut self) {
        let Some(callback) = self.callback.take() else {
            return;
        };
        let context = ptr::from_ref(callback.as_ref()).cast_mut().cast::<c_void>();
        // SAFETY: all callback storage is still owned here. Dispose is attempted
        // even if stop/uninitialize fail, including partial initialization.
        let disposed = unsafe {
            AudioOutputUnitStop(self.unit);
            AudioUnitUninitialize(self.unit);
            if self.error_listener {
                AudioUnitRemovePropertyListenerWithUserData(
                    self.unit,
                    kAudioUnitProperty_LastRenderError,
                    Some(render_error),
                    context,
                );
            }
            AudioComponentInstanceDispose(self.unit)
        };
        if disposed != 0 {
            // A failed disposal cannot prove that native code released its
            // callback pointers. Fail closed rather than free live callback
            // storage or leak it indefinitely.
            std::process::abort();
        }
        // On success, callback (including consumer blocks/Arcs) drops only now,
        // after native rendering and notifications can no longer reference it.
    }
}

unsafe extern "C-unwind" fn render_error(
    context: NonNull<c_void>,
    _unit: AudioUnit,
    _property: u32,
    _scope: u32,
    _element: u32,
) {
    // SAFETY: the registration context survives until after unit disposal.
    let state = unsafe { context.cast::<CallbackState>().as_ref() };
    state.render_failed.store(true, Ordering::Release);
    state
        .shared
        .faults
        .fetch_or(BACKEND_FAILURE, Ordering::Release);
}

unsafe extern "C-unwind" fn render(
    context: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    timestamp: NonNull<AudioTimeStamp>,
    bus: u32,
    frame_count: u32,
    data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: this single unit serializes render callbacks. No owner/listener
    // accesses render, and its enclosing allocation survives unit disposal.
    let state = unsafe { context.cast::<CallbackState>().as_ref() };
    let render = unsafe { &mut *state.render.get() };
    let Some(bytes) = frame_count.checked_mul(8) else {
        return invalid_buffer(render);
    };
    // SAFETY: CoreAudio supplies a valid buffer list or null. Only its first
    // buffer is accessed, after verifying the negotiated interleaved layout.
    let Some(data) = (unsafe { data.as_mut() }) else {
        return invalid_buffer(render);
    };
    if bus != 0 || data.mNumberBuffers != 1 {
        return invalid_buffer(render);
    }
    let buffer = &mut data.mBuffers[0];
    if buffer.mNumberChannels != 2
        || buffer.mDataByteSize < bytes
        || (bytes != 0
            && (buffer.mData.is_null() || !buffer.mData.addr().is_multiple_of(align_of::<f32>())))
    {
        return invalid_buffer(render);
    }
    let samples = if bytes == 0 {
        &mut []
    } else {
        // SAFETY: the byte size and alignment were checked above; CoreAudio
        // exclusively lends these writable samples for this render call.
        unsafe {
            std::slice::from_raw_parts_mut(
                buffer.mData.cast::<f32>(),
                bytes as usize / size_of::<f32>(),
            )
        }
    };
    let frames = render.consumer.consume(samples, true);
    samples[frames * 2..].fill(0.0);
    buffer.mDataByteSize = bytes;

    // Both clocks are CoreAudio host nanoseconds, not the potentially reset
    // sample clock. Reading/converting host time does not query device state.
    let now = unsafe { AudioConvertHostTimeToNanos(AudioGetCurrentHostTime()) };
    if frames != 0 {
        // SAFETY: timestamp is the valid borrowed timestamp for this callback.
        let timestamp = unsafe { timestamp.as_ref() };
        let end = if timestamp
            .mFlags
            .contains(AudioTimeStampFlags::HostTimeValid)
        {
            let start = unsafe { AudioConvertHostTimeToNanos(timestamp.mHostTime) };
            start.checked_add(frame_duration(frames).as_nanos() as u64)
        } else {
            None
        };
        let Some(end) = end else {
            state
                .shared
                .faults
                .fetch_or(TIMESTAMP_OVERFLOW, Ordering::Release);
            render.consumer.publish(false);
            return kAudio_ParamError;
        };
        render.last_sample_end = Some(
            render
                .last_sample_end
                .map_or(end, |previous| previous.max(end)),
        );
    }
    if let Some(end) = render.last_sample_end {
        let Some(deadline) = end.checked_add(state.latency_nanos.load(Ordering::Acquire)) else {
            state
                .shared
                .faults
                .fetch_or(TIMESTAMP_OVERFLOW, Ordering::Release);
            render.consumer.publish(false);
            return kAudio_ParamError;
        };
        // Keep an already scheduled deadline when a device change reduces the
        // latency; also account for increased latency while draining silence.
        render.last_playback_end = render.last_playback_end.max(deadline);
    }
    render.consumer.publish(now >= render.last_playback_end);
    0
}

fn invalid_buffer(render: &RenderState) -> i32 {
    render
        .consumer
        .shared
        .faults
        .fetch_or(INVALID_DEVICE_BUFFER, Ordering::Release);
    render.consumer.publish(false);
    // The unit must not process an invalid buffer; returning an error is safer
    // than writing silence through an invalid/null/misaligned destination.
    kAudio_ParamError
}

fn status(code: i32, operation: &str) -> Result<()> {
    ensure!(
        code == 0,
        "{operation}: OSStatus {code} (0x{:08x})",
        code as u32
    );
    Ok(())
}

// SAFETY: callers must pair each property with its documented Copy value type.
unsafe fn unit_property<T: Copy>(unit: AudioUnit, property: u32) -> Result<T> {
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    let code = unsafe {
        AudioUnitGetProperty(
            unit,
            property,
            kAudioUnitScope_Global,
            0,
            NonNull::from(&mut value).cast(),
            NonNull::from(&mut size),
        )
    };
    ensure!(
        code == 0,
        "reading CoreAudio unit property {property:#x}: OSStatus {code}"
    );
    ensure!(
        size as usize == size_of::<T>(),
        "CoreAudio unit property {property:#x} returned an invalid size"
    );
    // SAFETY: successful scalar query initialized exactly the expected type.
    Ok(unsafe { value.assume_init() })
}

// SAFETY: callers must pair each property with its documented Copy value type.
unsafe fn device_property<T: Copy>(device: u32, property: u32, scope: u32) -> Result<T> {
    let address = AudioObjectPropertyAddress {
        mSelector: property,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    let code = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    ensure!(
        code == 0,
        "reading CoreAudio device {device} property {property:#x}: OSStatus {code}"
    );
    ensure!(
        size as usize == size_of::<T>(),
        "CoreAudio device property {property:#x} returned an invalid size"
    );
    // SAFETY: successful scalar query initialized exactly the expected type.
    Ok(unsafe { value.assume_init() })
}
