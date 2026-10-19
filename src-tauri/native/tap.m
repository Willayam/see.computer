#import <AudioToolbox/AudioToolbox.h>
#import <CoreAudio/AudioHardware.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/HostTime.h>
#import <CoreAudio/CATapDescription.h>
#import <Foundation/Foundation.h>
#include <dlfcn.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

typedef struct {
    AudioObjectID tap;
    AudioObjectID device;
    AudioDeviceIOProcID proc;
    ExtAudioFileRef file;
    bool started;
    UInt64 start_host;
    Float64 rate;
    UInt64 written;
    AudioBufferList *zeros;
    UInt32 zero_frames;
    UInt32 bytes_per_frame;
    void *queue;
    _Atomic bool stopping;
} SeeTap;

// A tap delivers nothing while the tapped app plays nothing, so the file is
// padded to the clock; otherwise "them" drifts against "you" after every pause.
static void see_pad_to(SeeTap *tap, UInt64 target) {
    while (tap->written < target) {
        UInt32 frames = (UInt32)MIN((UInt64)tap->zero_frames, target - tap->written);
        for (UInt32 i = 0; i < tap->zeros->mNumberBuffers; i++) {
            tap->zeros->mBuffers[i].mDataByteSize = frames * tap->bytes_per_frame;
        }
        ExtAudioFileWrite(tap->file, frames, tap->zeros);
        tap->written += frames;
    }
}

static UInt64 see_frames_at(SeeTap *tap, UInt64 host) {
    if (host <= tap->start_host) return 0;
    return (UInt64)((Float64)AudioConvertHostTimeToNanos(host - tap->start_host) * tap->rate / 1e9);
}

static void see_error(char *error, int32_t length, const char *step, OSStatus status) {
    if (error && length > 0) snprintf(error, (size_t)length, "%s failed with OSStatus %d", step, (int)status);
}

static void *see_tcc(const char *symbol) {
    static void *framework;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        framework = dlopen("/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC", RTLD_NOW);
    });
    return framework ? dlsym(framework, symbol) : NULL;
}

// 0 granted, 1 denied, 2 not yet decided, 3 cannot tell. A tap made while the
// answer is pending records silence, and on 2026-10-06 one left coreaudiod
// unable to start any audio until it was restarted, so callers tap only on 0 or 3.
int32_t see_tap_permission(void) {
    int (*preflight)(CFStringRef, CFDictionaryRef) = see_tcc("TCCAccessPreflight");
    if (!preflight) return 3;
    int result = preflight(CFSTR("kTCCServiceAudioCapture"), NULL);
    return result == 0 ? 0 : result == 1 ? 1 : 2;
}

void see_tap_request_permission(void) {
    void (*request)(CFStringRef, CFDictionaryRef, void (^)(Boolean)) = see_tcc("TCCAccessRequest");
    if (request) request(CFSTR("kTCCServiceAudioCapture"), NULL, ^(Boolean granted __unused) {});
}

static void see_tap_teardown(SeeTap *tap) {
    if (tap->started) AudioDeviceStop(tap->device, tap->proc);
    if (tap->proc) AudioDeviceDestroyIOProcID(tap->device, tap->proc);
    if (tap->device) AudioHardwareDestroyAggregateDevice(tap->device);
    if (tap->tap) {
        if (@available(macOS 14.4, *)) AudioHardwareDestroyProcessTap(tap->tap);
    }
    free(tap->zeros);
    if (tap->queue) CFRelease(tap->queue);
    free(tap);
}

// The WAV is finished without calling into the HAL, and the HAL teardown runs
// in the background: while macOS waits on a permission answer, coreaudiod can
// hold every call from this process, and the caller must not hang with it.
void see_tap_stop(void *opaque) {
    if (!opaque) return;
    SeeTap *tap = opaque;
    atomic_store(&tap->stopping, true);
    if (tap->queue) dispatch_sync((__bridge dispatch_queue_t)tap->queue, ^{});
    if (tap->file) {
        if (tap->started) see_pad_to(tap, see_frames_at(tap, AudioGetCurrentHostTime()));
        ExtAudioFileDispose(tap->file);
        tap->file = NULL;
    }
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{ see_tap_teardown(tap); });
}

void *see_tap_start(const uint32_t *objects, int32_t count, const char *wav_path, char *error, int32_t error_len) {
    if (error && error_len > 0) error[0] = '\0';
    if (@available(macOS 14.4, *)) {
        SeeTap *tap = calloc(1, sizeof(*tap));
        if (!tap) {
            if (error && error_len > 0) snprintf(error, (size_t)error_len, "Allocating tap failed");
            return NULL;
        }
        OSStatus status = noErr;
        const char *step = NULL;
        NSString *uid = nil;
        NSDictionary *configuration = nil;
        NSURL *url = nil;
        dispatch_queue_t queue = nil;
        NSMutableArray<NSNumber *> *processes = [NSMutableArray array];
        if (count > 0) {
            for (int32_t i = 0; i < count; i++) [processes addObject:@(objects[i])];
        } else {
            pid_t pid = getpid();
            AudioObjectID own = kAudioObjectUnknown;
            UInt32 size = sizeof(own);
            AudioObjectPropertyAddress address = {kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
            status = AudioObjectGetPropertyData(kAudioObjectSystemObject, &address, sizeof(pid), &pid, &size, &own);
            if (status == noErr && own != kAudioObjectUnknown) [processes addObject:@(own)];
        }
        CATapDescription *description = count > 0
            ? [[CATapDescription alloc] initStereoMixdownOfProcesses:processes]
            : [[CATapDescription alloc] initStereoGlobalTapButExcludeProcesses:processes];
        description.privateTap = YES;
        description.muteBehavior = CATapUnmuted;
        description.UUID = [NSUUID UUID];
        status = AudioHardwareCreateProcessTap(description, &tap->tap);
        if (status != noErr) { step = "AudioHardwareCreateProcessTap"; goto fail; }

        AudioStreamBasicDescription tap_format = {0};
        UInt32 size = sizeof(tap_format);
        AudioObjectPropertyAddress address = {kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
        status = AudioObjectGetPropertyData(tap->tap, &address, 0, NULL, &size, &tap_format);
        if (status != noErr) { step = "kAudioTapPropertyFormat"; goto fail; }

        AudioObjectID output = kAudioObjectUnknown;
        size = sizeof(output);
        address.mSelector = kAudioHardwarePropertyDefaultSystemOutputDevice;
        status = AudioObjectGetPropertyData(kAudioObjectSystemObject, &address, 0, NULL, &size, &output);
        if (status != noErr) { step = "kAudioHardwarePropertyDefaultSystemOutputDevice"; goto fail; }
        CFStringRef output_uid = NULL;
        size = sizeof(output_uid);
        address.mSelector = kAudioDevicePropertyDeviceUID;
        status = AudioObjectGetPropertyData(output, &address, 0, NULL, &size, &output_uid);
        if (status != noErr) { step = "kAudioDevicePropertyDeviceUID"; goto fail; }
        uid = (__bridge_transfer NSString *)output_uid;
        configuration = @{
            @kAudioAggregateDeviceNameKey: @"see.computer meeting tap",
            @kAudioAggregateDeviceUIDKey: [NSUUID UUID].UUIDString,
            @kAudioAggregateDeviceIsPrivateKey: @YES,
            @kAudioAggregateDeviceIsStackedKey: @NO,
            @kAudioAggregateDeviceTapAutoStartKey: @YES,
            @kAudioAggregateDeviceMainSubDeviceKey: uid,
            @kAudioAggregateDeviceSubDeviceListKey: @[@{@kAudioSubDeviceUIDKey: uid}],
            @kAudioAggregateDeviceTapListKey: @[@{@kAudioSubTapUIDKey: description.UUID.UUIDString,
                                                  @kAudioSubTapDriftCompensationKey: @YES}]
        };
        status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)configuration, &tap->device);
        if (status != noErr) { step = "AudioHardwareCreateAggregateDevice"; goto fail; }

        AudioStreamBasicDescription file_format = {0};
        file_format.mSampleRate = 16000;
        file_format.mFormatID = kAudioFormatLinearPCM;
        file_format.mFormatFlags = kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked;
        file_format.mBytesPerPacket = 2;
        file_format.mFramesPerPacket = 1;
        file_format.mBytesPerFrame = 2;
        file_format.mChannelsPerFrame = 1;
        file_format.mBitsPerChannel = 16;
        url = [NSURL fileURLWithFileSystemRepresentation:wav_path isDirectory:NO relativeToURL:nil];
        status = ExtAudioFileCreateWithURL((__bridge CFURLRef)url, kAudioFileWAVEType, &file_format, NULL, kAudioFileFlags_EraseFile, &tap->file);
        if (status != noErr) { step = "ExtAudioFileCreateWithURL"; goto fail; }
        status = ExtAudioFileSetProperty(tap->file, kExtAudioFileProperty_ClientDataFormat, sizeof(tap_format), &tap_format);
        if (status != noErr) { step = "kExtAudioFileProperty_ClientDataFormat"; goto fail; }
        UInt32 buffers = (tap_format.mFormatFlags & kAudioFormatFlagIsNonInterleaved) ? tap_format.mChannelsPerFrame : 1;
        tap->rate = tap_format.mSampleRate;
        tap->bytes_per_frame = tap_format.mBytesPerFrame;
        tap->zero_frames = 4096;
        tap->zeros = calloc(1, offsetof(AudioBufferList, mBuffers) + sizeof(AudioBuffer) * buffers + (size_t)tap->zero_frames * tap->bytes_per_frame * buffers);
        if (!tap->zeros) { step = "Allocating silence"; status = -1; goto fail; }
        tap->zeros->mNumberBuffers = buffers;
        char *silence = (char *)&tap->zeros->mBuffers[buffers];
        for (UInt32 i = 0; i < buffers; i++) {
            tap->zeros->mBuffers[i].mNumberChannels = buffers == 1 ? tap_format.mChannelsPerFrame : 1;
            tap->zeros->mBuffers[i].mData = silence + (size_t)i * tap->zero_frames * tap->bytes_per_frame;
        }
        queue = dispatch_queue_create("see.computer.meeting.tap", DISPATCH_QUEUE_SERIAL);
        tap->queue = (void *)CFBridgingRetain(queue);
        status = AudioDeviceCreateIOProcIDWithBlock(&tap->proc, tap->device, queue,
            ^(const AudioTimeStamp *now __unused, const AudioBufferList *input,
              const AudioTimeStamp *input_time, AudioBufferList *output __unused,
              const AudioTimeStamp *output_time __unused) {
                if (atomic_load(&tap->stopping)) return;
                if (input_time->mFlags & kAudioTimeStampHostTimeValid) {
                    UInt64 due = see_frames_at(tap, input_time->mHostTime);
                    if (due > tap->written + (UInt64)(tap->rate / 10)) see_pad_to(tap, due);
                }
                UInt32 frames = input->mBuffers[0].mDataByteSize / tap_format.mBytesPerFrame;
                ExtAudioFileWrite(tap->file, frames, input);
                tap->written += frames;
            });
        if (status != noErr) { step = "AudioDeviceCreateIOProcIDWithBlock"; goto fail; }
        tap->start_host = AudioGetCurrentHostTime();
        status = AudioDeviceStart(tap->device, tap->proc);
        if (status != noErr) { step = "AudioDeviceStart"; goto fail; }
        tap->started = true;
        return tap;
    fail:
        see_error(error, error_len, step, status);
        see_tap_stop(tap);
        return NULL;
    }
    if (error && error_len > 0) snprintf(error, (size_t)error_len, "Recording call audio needs macOS 14.4 or later");
    return NULL;
}
