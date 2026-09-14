// ScreenCaptureKit display recording with independently captured microphone audio.
// Rust owns one retained SeeRecording handle and asks it to stop on a worker thread.

#import <AVFoundation/AVFoundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>
#import <VideoToolbox/VideoToolbox.h>
#include <math.h>
#include <unistd.h>

static NSString *const SeeRecorderErrorDomain = @"computer.see.recorder";

static NSError *SeeError(NSInteger code, NSString *message) {
    return [NSError errorWithDomain:SeeRecorderErrorDomain
                               code:code
                           userInfo:@{NSLocalizedDescriptionKey : message}];
}

static void SeeCopyError(char *buffer, size_t capacity, NSString *message) {
    if (buffer == NULL || capacity == 0) return;
    const char *text = message.UTF8String ?: "native recorder failed";
    snprintf(buffer, capacity, "%s", text);
}

typedef void (^SeeStopCompletion)(NSError *_Nullable error);

@interface SeeRecording : NSObject <SCStreamOutput, SCStreamDelegate,
                                    AVCaptureAudioDataOutputSampleBufferDelegate,
                                    AVAssetWriterDelegate>
@property(nonatomic, strong) NSURL *movieURL;
@property(nonatomic, strong) NSURL *segmentURL;
@property(nonatomic) dispatch_queue_t controlQueue;
@property(nonatomic) dispatch_queue_t sampleQueue;
@property(nonatomic) dispatch_queue_t segmentQueue;
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) AVCaptureSession *audioSession;
@property(nonatomic, strong) AVCaptureAudioDataOutput *audioOutput;
@property(nonatomic, strong) AVAssetWriter *writer;
@property(nonatomic, strong) AVAssetWriterInput *videoInput;
@property(nonatomic, strong) AVAssetWriterInput *audioInput;
@property(nonatomic) CMSampleBufferRef lastVideoSample;
@property(nonatomic) dispatch_source_t videoTimer;
@property(nonatomic, strong) NSFileHandle *movieHandle;
@property(nonatomic, strong) NSMutableArray<NSNumber *> *segmentDurations;
@property(nonatomic, strong) NSMutableArray<SeeStopCompletion> *stopCompletions;
@property(nonatomic, strong) id runtimeErrorObserver;
@property(nonatomic, strong) SeeRecording *keepAlive;
@property(atomic) CMTime hostOrigin;
@property(nonatomic) NSUInteger segmentCount;
@property(atomic) BOOL acceptingSamples;
@property(atomic) BOOL setupPending;
@property(atomic) BOOL stopping;
@property(atomic) BOOL cancelRequested;
@property(atomic) BOOL completed;
@property(nonatomic) BOOL shutdownStarted;
@property(nonatomic, strong) NSError *terminalError;
- (instancetype)initWithMoviePath:(NSString *)moviePath segmentPath:(NSString *)segmentPath;
- (void)start;
- (void)requestStop:(BOOL)cancel completion:(SeeStopCompletion _Nullable)completion;
@end

@implementation SeeRecording

- (instancetype)initWithMoviePath:(NSString *)moviePath segmentPath:(NSString *)segmentPath {
    self = [super init];
    if (self) {
        _movieURL = [NSURL fileURLWithPath:moviePath];
        _segmentURL = [NSURL fileURLWithPath:segmentPath];
        _controlQueue = dispatch_queue_create("computer.see.recorder.control", DISPATCH_QUEUE_SERIAL);
        _sampleQueue = dispatch_queue_create("computer.see.recorder.samples", DISPATCH_QUEUE_SERIAL);
        _segmentQueue = dispatch_queue_create("computer.see.recorder.segments", DISPATCH_QUEUE_SERIAL);
        _segmentDurations = [NSMutableArray array];
        _stopCompletions = [NSMutableArray array];
        _hostOrigin = kCMTimeInvalid;
        _setupPending = YES;
        _keepAlive = self;
    }
    return self;
}

- (void)start {
    dispatch_async(self.controlQueue, ^{
        [self requestMicrophoneAccess];
    });
}

- (void)requestMicrophoneAccess {
    if (self.completed) return;
    AVAuthorizationStatus status = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
    if (status == AVAuthorizationStatusDenied || status == AVAuthorizationStatusRestricted) {
        [self failSetup:SeeError(10, @"Microphone permission is off for see.computer")];
        return;
    }
    if (status == AVAuthorizationStatusNotDetermined) {
        [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio
                                completionHandler:^(BOOL granted) {
            dispatch_async(self.controlQueue, ^{
                if (self.completed) return;
                if (!granted) {
                    [self failSetup:SeeError(11, @"Microphone permission was not granted")];
                } else {
                    [self loadShareableContent];
                }
            });
        }];
        return;
    }
    [self loadShareableContent];
}

- (void)loadShareableContent {
    if (self.completed) return;
    [SCShareableContent getShareableContentExcludingDesktopWindows:NO
                                               onScreenWindowsOnly:YES
                                                 completionHandler:^(SCShareableContent *content,
                                                                     NSError *error) {
        dispatch_async(self.controlQueue, ^{
            if (self.completed) return;
            if (error != nil) {
                [self failSetup:error];
                return;
            }
            [self configureWithContent:content];
        });
    }];
}

- (void)configureWithContent:(SCShareableContent *)content {
    if (self.completed || self.stopping) {
        [self finishControlWithError:SeeError(12, @"Recording stopped before setup completed")];
        return;
    }

    CGDirectDisplayID mainDisplayID = CGMainDisplayID();
    SCDisplay *display = nil;
    for (SCDisplay *candidate in content.displays) {
        if (candidate.displayID == mainDisplayID) {
            display = candidate;
            break;
        }
    }
    if (display == nil) {
        [self failSetup:SeeError(13, @"The main display is unavailable to ScreenCaptureKit")];
        return;
    }

    NSError *error = nil;
    if (![self prepareFiles:&error] || ![self prepareMicrophone:&error] ||
        ![self prepareWriterForDisplay:display error:&error]) {
        [self failSetup:error ?: SeeError(14, @"Recorder setup failed")];
        return;
    }

    SCRunningApplication *ownApplication = nil;
    for (SCRunningApplication *application in content.applications) {
        if (application.processID == getpid()) {
            ownApplication = application;
            break;
        }
    }
    SCContentFilter *filter;
    if (ownApplication != nil) {
        filter = [[SCContentFilter alloc] initWithDisplay:display
                                    excludingApplications:@[ ownApplication ]
                                         exceptingWindows:@[]];
    } else {
        NSArray<SCWindow *> *ownWindows = [content.windows
            filteredArrayUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(SCWindow *window,
                                                                               NSDictionary *_) {
                return window.owningApplication.processID == getpid();
            }]];
        filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:ownWindows];
    }

    SCStreamConfiguration *configuration = [SCStreamConfiguration new];
    double scale = MIN(1.0, MIN(2560.0 / MAX(1, display.width),
                                1440.0 / MAX(1, display.height)));
    NSInteger width = MAX(2, (NSInteger)floor(display.width * scale / 2.0) * 2);
    NSInteger height = MAX(2, (NSInteger)floor(display.height * scale / 2.0) * 2);
    configuration.width = width;
    configuration.height = height;
    configuration.minimumFrameInterval = CMTimeMake(1, 15);
    configuration.queueDepth = 3;
    configuration.showsCursor = YES;
    configuration.capturesAudio = NO;
    configuration.preservesAspectRatio = YES;
    configuration.pixelFormat = kCVPixelFormatType_32BGRA;

    self.stream = [[SCStream alloc] initWithFilter:filter configuration:configuration delegate:self];
    if (![self.stream addStreamOutput:self
                                 type:SCStreamOutputTypeScreen
                   sampleHandlerQueue:self.sampleQueue
                                error:&error]) {
        [self failSetup:error ?: SeeError(15, @"Could not attach the screen capture output")];
        return;
    }

    self.hostOrigin = CMClockGetTime(CMClockGetHostTimeClock());
    self.writer.initialSegmentStartTime = kCMTimeZero;
    if (![self.writer startWriting]) {
        [self failSetup:self.writer.error ?: SeeError(16, @"Could not start the hardware H.264 encoder")];
        return;
    }
    [self.writer startSessionAtSourceTime:kCMTimeZero];
    self.acceptingSamples = YES;
    [self startVideoTimer];
    [self.audioSession startRunning];
    if (!self.audioSession.isRunning) {
        [self failSetup:SeeError(17, @"The default microphone did not start")];
        return;
    }

    self.setupPending = NO;
    [self.stream startCaptureWithCompletionHandler:^(NSError *startError) {
        dispatch_async(self.controlQueue, ^{
            if (self.completed) return;
            if (startError != nil) {
                [self failSetup:startError];
            } else if (self.stopping) {
                [self beginShutdown];
            }
        });
    }];
}

- (void)startVideoTimer {
    // ScreenCaptureKit stops sending complete frames when the screen is still.
    // Repeat its latest frame so audio and one-second fragments keep advancing.
    dispatch_async(self.sampleQueue, ^{
        self.videoTimer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, self.sampleQueue);
        dispatch_source_set_timer(self.videoTimer, DISPATCH_TIME_NOW, NSEC_PER_SEC / 15, NSEC_PER_MSEC * 2);
        dispatch_source_set_event_handler(self.videoTimer, ^{
            if (!self.acceptingSamples || self.lastVideoSample == NULL) return;
            CMSampleTimingInfo timing = {
                .duration = CMTimeMake(1, 15),
                .presentationTimeStamp = CMClockGetTime(CMClockGetHostTimeClock()),
                .decodeTimeStamp = kCMTimeInvalid,
            };
            CMSampleBufferRef frame = NULL;
            if (CMSampleBufferCreateCopyWithNewTiming(kCFAllocatorDefault, self.lastVideoSample,
                                                      1, &timing, &frame) == noErr && frame != NULL) {
                [self appendSampleBuffer:frame toInput:self.videoInput];
                CFRelease(frame);
            }
        });
        dispatch_resume(self.videoTimer);
    });
}

- (BOOL)prepareFiles:(NSError **)error {
    NSFileManager *files = NSFileManager.defaultManager;
    if (![files createDirectoryAtURL:self.segmentURL
          withIntermediateDirectories:YES
                           attributes:nil
                                error:error]) return NO;
    if (![files fileExistsAtPath:self.movieURL.path] &&
        ![files createFileAtPath:self.movieURL.path contents:nil attributes:nil]) {
        if (error != NULL) *error = SeeError(18, @"Could not create the recording movie");
        return NO;
    }
    self.movieHandle = [NSFileHandle fileHandleForWritingToURL:self.movieURL error:error];
    if (self.movieHandle != nil && ![self.movieHandle truncateAtOffset:0 error:error]) return NO;
    return self.movieHandle != nil;
}

- (BOOL)prepareMicrophone:(NSError **)error {
    AVCaptureDevice *microphone = [AVCaptureDevice defaultDeviceWithMediaType:AVMediaTypeAudio];
    if (microphone == nil) {
        if (error != NULL) *error = SeeError(19, @"No default microphone is available");
        return NO;
    }
    AVCaptureDeviceInput *input = [AVCaptureDeviceInput deviceInputWithDevice:microphone error:error];
    if (input == nil) return NO;

    AVCaptureSession *session = [AVCaptureSession new];
    AVCaptureAudioDataOutput *output = [AVCaptureAudioDataOutput new];
    output.audioSettings = @{
        AVFormatIDKey : @(kAudioFormatLinearPCM),
        AVSampleRateKey : @48000,
        AVNumberOfChannelsKey : @1,
        AVLinearPCMBitDepthKey : @16,
        AVLinearPCMIsFloatKey : @NO,
        AVLinearPCMIsNonInterleaved : @NO,
    };
    [output setSampleBufferDelegate:self queue:self.sampleQueue];
    [session beginConfiguration];
    if (![session canAddInput:input] || ![session canAddOutput:output]) {
        [session commitConfiguration];
        if (error != NULL) *error = SeeError(20, @"The default microphone cannot supply recorder audio");
        return NO;
    }
    [session addInput:input];
    [session addOutput:output];
    [session commitConfiguration];
    self.audioSession = session;
    self.audioOutput = output;

    self.runtimeErrorObserver = [NSNotificationCenter.defaultCenter
        addObserverForName:AVCaptureSessionRuntimeErrorNotification
                    object:session
                     queue:nil
                usingBlock:^(NSNotification *notification) {
        NSError *runtimeError = notification.userInfo[AVCaptureSessionErrorKey];
        dispatch_async(self.controlQueue, ^{
            [self failSetup:runtimeError ?: SeeError(21, @"Microphone capture stopped")];
        });
    }];
    return YES;
}

- (BOOL)prepareWriterForDisplay:(SCDisplay *)display error:(NSError **)error {
    double scale = MIN(1.0, MIN(2560.0 / MAX(1, display.width),
                                1440.0 / MAX(1, display.height)));
    NSInteger width = MAX(2, (NSInteger)floor(display.width * scale / 2.0) * 2);
    NSInteger height = MAX(2, (NSInteger)floor(display.height * scale / 2.0) * 2);

    AVAssetWriter *writer = [[AVAssetWriter alloc] initWithContentType:UTTypeMPEG4Movie];
    writer.outputFileTypeProfile = AVFileTypeProfileMPEG4AppleHLS;
    writer.preferredOutputSegmentInterval = CMTimeMake(1, 1);
    writer.delegate = self;

    NSDictionary *videoSettings = @{
        AVVideoCodecKey : AVVideoCodecTypeH264,
        AVVideoWidthKey : @(width),
        AVVideoHeightKey : @(height),
        AVVideoEncoderSpecificationKey : @{
            (__bridge NSString *)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder : @YES,
        },
        AVVideoCompressionPropertiesKey : @{
            AVVideoAverageBitRateKey : @2000000,
            AVVideoExpectedSourceFrameRateKey : @15,
            AVVideoMaxKeyFrameIntervalKey : @15,
            AVVideoMaxKeyFrameIntervalDurationKey : @1.0,
            AVVideoAllowFrameReorderingKey : @NO,
        },
    };
    AVAssetWriterInput *videoInput = [AVAssetWriterInput assetWriterInputWithMediaType:AVMediaTypeVideo
                                                                        outputSettings:videoSettings];
    videoInput.expectsMediaDataInRealTime = YES;

    NSDictionary *audioSettings = @{
        AVFormatIDKey : @(kAudioFormatMPEG4AAC),
        AVSampleRateKey : @48000,
        AVNumberOfChannelsKey : @1,
        AVEncoderBitRateKey : @64000,
    };
    AVAssetWriterInput *audioInput = [AVAssetWriterInput assetWriterInputWithMediaType:AVMediaTypeAudio
                                                                        outputSettings:audioSettings];
    audioInput.expectsMediaDataInRealTime = YES;
    if (![writer canAddInput:videoInput] || ![writer canAddInput:audioInput]) {
        if (error != NULL) *error = SeeError(22, @"The fragmented MP4 writer rejected its audio or video input");
        return NO;
    }
    [writer addInput:videoInput];
    [writer addInput:audioInput];
    self.writer = writer;
    self.videoInput = videoInput;
    self.audioInput = audioInput;
    return YES;
}

- (void)stream:(SCStream *)stream
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
                   ofType:(SCStreamOutputType)type {
    if (type != SCStreamOutputTypeScreen || !CMSampleBufferIsValid(sampleBuffer)) return;
    CFArrayRef attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, false);
    if (attachments == NULL || CFArrayGetCount(attachments) == 0) return;
    CFDictionaryRef first = CFArrayGetValueAtIndex(attachments, 0);
    CFNumberRef status = CFDictionaryGetValue(first, (__bridge const void *)SCStreamFrameInfoStatus);
    int value = -1;
    if (status == NULL || !CFNumberGetValue(status, kCFNumberIntType, &value) ||
        value != SCFrameStatusComplete) return;
    if (!self.acceptingSamples) return;
    if (self.lastVideoSample != NULL) CFRelease(self.lastVideoSample);
    self.lastVideoSample = (CMSampleBufferRef)CFRetain(sampleBuffer);
}

- (void)captureOutput:(AVCaptureOutput *)output
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
           fromConnection:(AVCaptureConnection *)connection {
    if (output != self.audioOutput || !CMSampleBufferIsValid(sampleBuffer)) return;
    [self appendSampleBuffer:sampleBuffer toInput:self.audioInput];
}

- (void)appendSampleBuffer:(CMSampleBufferRef)sampleBuffer toInput:(AVAssetWriterInput *)input {
    if (!self.acceptingSamples || !CMTIME_IS_NUMERIC(self.hostOrigin)) return;
    CMTime pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer);
    if (!CMTIME_IS_NUMERIC(pts)) return;
    CMTime rebasedPTS = CMTimeSubtract(pts, self.hostOrigin);
    if (CMTIME_COMPARE_INLINE(rebasedPTS, <, kCMTimeZero)) return;

    // PCM timing uses the duration of one audio sample, not the entire buffer.
    CMItemCount count = 0;
    if (CMSampleBufferGetSampleTimingInfoArray(sampleBuffer, 0, NULL, &count) != noErr ||
        count < 1 || count > 4096) return;
    CMSampleTimingInfo single;
    CMSampleTimingInfo *timings = count == 1 ? &single : calloc((size_t)count, sizeof(*timings));
    if (timings == NULL) return;
    if (CMSampleBufferGetSampleTimingInfoArray(sampleBuffer, count, timings, NULL) != noErr) {
        if (count != 1) free(timings);
        return;
    }
    for (CMItemCount i = 0; i < count; i++) {
        timings[i].presentationTimeStamp = CMTimeSubtract(timings[i].presentationTimeStamp, self.hostOrigin);
        if (CMTIME_IS_NUMERIC(timings[i].decodeTimeStamp)) {
            timings[i].decodeTimeStamp = CMTimeSubtract(timings[i].decodeTimeStamp, self.hostOrigin);
        }
    }
    CMSampleBufferRef rebased = NULL;
    OSStatus copyStatus = CMSampleBufferCreateCopyWithNewTiming(kCFAllocatorDefault,
                                                                sampleBuffer,
                                                                count,
                                                                timings,
                                                                &rebased);
    if (count != 1) free(timings);
    if (copyStatus != noErr || rebased == NULL) {
        dispatch_async(self.controlQueue, ^{
            [self failSetup:SeeError(23, @"Could not rebase a captured sample")];
        });
        return;
    }
    BOOL appended = !input.isReadyForMoreMediaData || [input appendSampleBuffer:rebased];
    CFRelease(rebased);
    if (!appended && self.writer.status == AVAssetWriterStatusFailed) {
        NSError *writerError = self.writer.error ?: SeeError(24, @"The media encoder stopped");
        dispatch_async(self.controlQueue, ^{
            [self failSetup:writerError];
        });
    }
}

- (void)assetWriter:(AVAssetWriter *)writer
    didOutputSegmentData:(NSData *)segmentData
              segmentType:(AVAssetSegmentType)segmentType
            segmentReport:(AVAssetSegmentReport *)segmentReport {
    dispatch_sync(self.segmentQueue, ^{
        if (self.cancelRequested || self.completed) return;
        NSString *name;
        if (segmentType == AVAssetSegmentTypeInitialization) {
            name = @"init.mp4";
        } else {
            name = [NSString stringWithFormat:@"segment-%03lu.m4s", (unsigned long)self.segmentCount];
        }
        NSURL *destination = [self.segmentURL URLByAppendingPathComponent:name];
        NSError *error = nil;
        if (![segmentData writeToURL:destination options:NSDataWritingAtomic error:&error] ||
            ![self.movieHandle writeData:segmentData error:&error]) {
            dispatch_async(self.controlQueue, ^{
                [self failSetup:error ?: SeeError(25, @"Could not persist a recording segment")];
            });
            return;
        }
        if (segmentType == AVAssetSegmentTypeSeparable) {
            NSTimeInterval duration = [self durationForReport:segmentReport];
            [self.segmentDurations addObject:@(duration)];
            self.segmentCount += 1;
        }
        if (![self writePlaylistEnding:NO error:&error]) {
            dispatch_async(self.controlQueue, ^{
                [self failSetup:error ?: SeeError(26, @"Could not update the recording playlist")];
            });
        }
    });
}

- (NSTimeInterval)durationForReport:(AVAssetSegmentReport *)report {
    NSTimeInterval longest = 0;
    for (AVAssetSegmentTrackReport *track in report.trackReports) {
        if (!CMTIME_IS_NUMERIC(track.duration)) continue;
        NSTimeInterval duration = CMTimeGetSeconds(track.duration);
        if (isfinite(duration)) longest = MAX(longest, duration);
    }
    return longest > 0 ? longest : 1.0;
}

- (BOOL)writePlaylistEnding:(BOOL)ending error:(NSError **)error {
    NSTimeInterval maximum = 1.0;
    for (NSNumber *duration in self.segmentDurations) maximum = MAX(maximum, duration.doubleValue);
    NSMutableString *playlist = [NSMutableString stringWithFormat:
        @"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:%ld\n"
         "#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-INDEPENDENT-SEGMENTS\n"
         "#EXT-X-MAP:URI=\"init.mp4\"\n",
        (long)ceil(maximum)];
    [self.segmentDurations enumerateObjectsUsingBlock:^(NSNumber *duration,
                                                         NSUInteger index,
                                                         BOOL *__unused stop) {
        [playlist appendFormat:@"#EXTINF:%.6f,\nsegment-%03lu.m4s\n",
                               duration.doubleValue, (unsigned long)index];
    }];
    if (ending) [playlist appendString:@"#EXT-X-ENDLIST\n"];
    NSURL *url = [self.segmentURL URLByAppendingPathComponent:@"index.m3u8"];
    return [playlist writeToURL:url atomically:YES encoding:NSUTF8StringEncoding error:error];
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    dispatch_async(self.controlQueue, ^{
        [self failSetup:error ?: SeeError(27, @"Screen capture stopped")];
    });
}

- (void)failSetup:(NSError *)error {
    if (self.completed || self.stopping) return;
    self.terminalError = error;
    self.stopping = YES;
    self.cancelRequested = YES;
    [self beginShutdown];
}

- (void)requestStop:(BOOL)cancel completion:(SeeStopCompletion)completion {
    dispatch_async(self.controlQueue, ^{
        if (completion != nil) [self.stopCompletions addObject:[completion copy]];
        if (self.completed) {
            [self drainStopCompletions];
            return;
        }
        self.cancelRequested = self.cancelRequested || cancel;
        if (self.stopping) return;
        self.stopping = YES;
        if (self.setupPending && self.stream == nil) {
            NSError *error = cancel ? nil : SeeError(28, @"Recording stopped before capture started");
            [self finishControlWithError:error];
            return;
        }
        [self beginShutdown];
    });
}

- (void)beginShutdown {
    if (self.shutdownStarted) return;
    self.shutdownStarted = YES;
    [self.audioOutput setSampleBufferDelegate:nil queue:NULL];
    [self.audioSession stopRunning];
    SCStream *stream = self.stream;
    if (stream == nil) {
        [self finishWriter];
        return;
    }
    self.stream = nil;
    [stream stopCaptureWithCompletionHandler:^(NSError *error) {
        dispatch_async(self.controlQueue, ^{
            if (error != nil && self.terminalError == nil && !self.cancelRequested) {
                self.terminalError = error;
            }
            [self finishWriter];
        });
    }];
}

- (void)finishWriter {
    dispatch_async(self.sampleQueue, ^{
        self.acceptingSamples = NO;
        if (self.videoTimer != nil) {
            dispatch_source_cancel(self.videoTimer);
            self.videoTimer = nil;
        }
        if (self.lastVideoSample != NULL) {
            CFRelease(self.lastVideoSample);
            self.lastVideoSample = NULL;
        }
        if (self.cancelRequested) {
            [self.writer cancelWriting];
            dispatch_async(self.controlQueue, ^{
                [self finishControlWithError:self.terminalError];
            });
            return;
        }
        [self.videoInput markAsFinished];
        [self.audioInput markAsFinished];
        [self.writer finishWritingWithCompletionHandler:^{
            NSError *writerError = self.writer.status == AVAssetWriterStatusCompleted
                ? nil
                : (self.writer.error ?: SeeError(29, @"The fragmented movie did not finish"));
            dispatch_async(self.controlQueue, ^{
                [self finishControlWithError:self.terminalError ?: writerError];
            });
        }];
    });
}

- (void)finishControlWithError:(NSError *)error {
    if (self.completed) return;
    self.setupPending = NO;
    self.terminalError = error;
    if (self.runtimeErrorObserver != nil) {
        [NSNotificationCenter.defaultCenter removeObserver:self.runtimeErrorObserver];
        self.runtimeErrorObserver = nil;
    }
    dispatch_sync(self.segmentQueue, ^{
        NSError *fileError = nil;
        if (!self.cancelRequested && error == nil) {
            [self writePlaylistEnding:YES error:&fileError];
            [self.movieHandle synchronizeAndReturnError:&fileError];
        }
        [self.movieHandle closeAndReturnError:&fileError];
        self.movieHandle = nil;
        if (fileError != nil && self.terminalError == nil) self.terminalError = fileError;
        if (self.cancelRequested || self.terminalError != nil) {
            [NSFileManager.defaultManager removeItemAtURL:self.movieURL error:nil];
            [NSFileManager.defaultManager removeItemAtURL:self.segmentURL error:nil];
        }
    });
    self.completed = YES;
    // AVAssetWriter forbids changing its delegate after writing starts, including
    // after completion or cancellation. The delegate is weak and needs no clearing.
    self.keepAlive = nil;
    [self drainStopCompletions];
}

- (void)drainStopCompletions {
    NSArray<SeeStopCompletion> *completions = [self.stopCompletions copy];
    [self.stopCompletions removeAllObjects];
    NSError *error = self.terminalError;
    for (SeeStopCompletion completion in completions) completion(error);
}

@end

void *see_recording_start(const char *movie_path, const char *segment_path) {
    @autoreleasepool {
        if (movie_path == NULL || segment_path == NULL) return NULL;
        NSString *movie = [NSString stringWithUTF8String:movie_path];
        NSString *segments = [NSString stringWithUTF8String:segment_path];
        if (movie == nil || segments == nil) return NULL;
        SeeRecording *recording = [[SeeRecording alloc] initWithMoviePath:movie segmentPath:segments];
        [recording start];
        return (__bridge_retained void *)recording;
    }
}

int see_recording_stop(void *handle, bool cancel, uint32_t timeout_ms,
                       char *error_buffer, size_t error_capacity) {
    @autoreleasepool {
        if (handle == NULL) {
            SeeCopyError(error_buffer, error_capacity, @"Native recorder handle is missing");
            return 1;
        }
        SeeRecording *recording = (__bridge SeeRecording *)handle;
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        __block NSError *result = nil;
        [recording requestStop:cancel completion:^(NSError *error) {
            result = error;
            dispatch_semaphore_signal(done);
        }];
        dispatch_time_t deadline = dispatch_time(DISPATCH_TIME_NOW, (int64_t)timeout_ms * NSEC_PER_MSEC);
        if (dispatch_semaphore_wait(done, deadline) != 0) {
            [recording requestStop:YES completion:nil];
            SeeCopyError(error_buffer, error_capacity, @"Native recorder stop timed out");
            return 2;
        }
        if (result != nil) {
            SeeCopyError(error_buffer, error_capacity, result.localizedDescription);
            return 1;
        }
        return 0;
    }
}

void see_recording_cancel(void *handle) {
    if (handle == NULL) return;
    SeeRecording *recording = (__bridge SeeRecording *)handle;
    [recording requestStop:YES completion:nil];
}

void see_recording_release(void *handle) {
    if (handle != NULL) CFBridgingRelease(handle);
}
