// Synthetic native media test. It never opens a microphone or captures a screen.
// Compile with the same frameworks as native/recording.m, then pass an output directory.
#import "../src-tauri/native/recording.m"

@interface TestDisplay : NSObject
@property(nonatomic) NSInteger width;
@property(nonatomic) NSInteger height;
@end
@implementation TestDisplay
@end

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc < 2 || argc > 3) return 2;
        BOOL cancel = argc == 3 && strcmp(argv[2], "cancel") == 0;
        BOOL fail = argc == 3 && strcmp(argv[2], "fail") == 0;
        if (argc == 3 && !cancel && !fail) return 2;
        NSString *root = [NSString stringWithUTF8String:argv[1]];
        SeeRecording *recording = [[SeeRecording alloc]
            initWithMoviePath:[root stringByAppendingPathComponent:@"clip.mov"]
                 segmentPath:[root stringByAppendingPathComponent:@"clip.segments"]];
        TestDisplay *display = [TestDisplay new];
        display.width = 1280; display.height = 720;
        NSError *error = nil;
        if (![recording prepareFiles:&error] || ![recording prepareWriterForDisplay:(SCDisplay *)display error:&error]) {
            fprintf(stderr, "%s\n", error.localizedDescription.UTF8String);
            return 3;
        }
        recording.hostOrigin = CMClockGetTime(CMClockGetHostTimeClock());
        recording.writer.initialSegmentStartTime = kCMTimeZero;
        if (![recording.writer startWriting]) return 4;
        [recording.writer startSessionAtSourceTime:kCMTimeZero];
        recording.acceptingSamples = YES;
        recording.setupPending = NO;

        CVPixelBufferRef pixel = NULL;
        CVPixelBufferCreate(NULL, 1280, 720, kCVPixelFormatType_32BGRA,
            (__bridge CFDictionaryRef)@{ (id)kCVPixelBufferIOSurfacePropertiesKey: @{} }, &pixel);
        CVPixelBufferLockBaseAddress(pixel, 0);
        memset(CVPixelBufferGetBaseAddress(pixel), 128, CVPixelBufferGetDataSize(pixel));
        CVPixelBufferUnlockBaseAddress(pixel, 0);
        CMVideoFormatDescriptionRef videoFormat = NULL;
        CMVideoFormatDescriptionCreateForImageBuffer(NULL, pixel, &videoFormat);
        CMSampleTimingInfo videoTime = { CMTimeMake(1, 15), recording.hostOrigin, kCMTimeInvalid };
        CMSampleBufferRef video = NULL;
        CMSampleBufferCreateReadyWithImageBuffer(NULL, pixel, videoFormat, &videoTime, &video);
        CFMutableDictionaryRef attachment = (CFMutableDictionaryRef)CFArrayGetValueAtIndex(CMSampleBufferGetSampleAttachmentsArray(video, true), 0);
        int complete = SCFrameStatusComplete;
        CFNumberRef status = CFNumberCreate(NULL, kCFNumberIntType, &complete);
        CFDictionarySetValue(attachment, (__bridge const void *)SCStreamFrameInfoStatus, status);
        // One static screen frame must keep producing video fragments for the entire test.
        dispatch_sync(recording.sampleQueue, ^{ [recording stream:(SCStream *)recording didOutputSampleBuffer:video ofType:SCStreamOutputTypeScreen]; });
        [recording startVideoTimer];
        CFRelease(status); CFRelease(video); CFRelease(videoFormat); CFRelease(pixel);

        AudioStreamBasicDescription format = { 48000, kAudioFormatLinearPCM,
            kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked, 2, 1, 2, 1, 16, 0 };
        CMAudioFormatDescriptionRef audioFormat = NULL;
        CMAudioFormatDescriptionCreate(NULL, &format, 0, NULL, 0, NULL, NULL, &audioFormat);
        for (int chunk = 0; chunk < 200; chunk++) {
            CMBlockBufferRef block = NULL;
            CMBlockBufferCreateWithMemoryBlock(NULL, NULL, 1920, NULL, NULL, 0, 1920, 0, &block);
            int16_t samples[960];
            for (int i = 0; i < 960; i++) samples[i] = (int16_t)(sin((chunk * 960 + i) * 2 * M_PI * 440 / 48000) * 3000);
            CMBlockBufferReplaceDataBytes(samples, block, 0, sizeof(samples));
            CMSampleTimingInfo timing = { CMTimeMake(1, 48000),
                CMTimeAdd(recording.hostOrigin, CMTimeMake(chunk * 960, 48000)), kCMTimeInvalid };
            size_t sampleSize = 2;
            CMSampleBufferRef audio = NULL;
            CMSampleBufferCreateReady(NULL, block, audioFormat, 960, 1, &timing, 1, &sampleSize, &audio);
            dispatch_sync(recording.sampleQueue, ^{ [recording appendSampleBuffer:audio toInput:recording.audioInput]; });
            CFRelease(audio); CFRelease(block);
            CMTime deadline = CMTimeAdd(recording.hostOrigin, CMTimeMake((chunk + 1) * 960, 48000));
            double wait = CMTimeGetSeconds(CMTimeSubtract(deadline, CMClockGetTime(CMClockGetHostTimeClock())));
            if (wait > 0) usleep((useconds_t)(wait * 1000000));
            if (chunk == 100 && recording.segmentCount < 1) {
                fprintf(stderr, "No segment before stop on a static screen\n");
                return 5;
            }
        }
        CFRelease(audioFormat);
        if (fail) {
            dispatch_sync(recording.controlQueue, ^{
                [recording failSetup:SeeError(99, @"Injected recorder failure")];
            });
        }
        char message[1024] = {0};
        int result = see_recording_stop((__bridge void *)recording, cancel, 5000, message, sizeof(message));
        // Stop's callback can wake this thread before the control queue finishes cleanup.
        // Wait for teardown too, so a late exception cannot escape a successful test.
        dispatch_sync(recording.controlQueue, ^{});
        printf("stop=%d segments=%lu error=%s\n", result, (unsigned long)recording.segmentCount, message);
        if (!recording.completed || recording.keepAlive != nil || recording.movieHandle != nil) return 6;
        if (fail ? (result != 1 || strcmp(message, "Injected recorder failure") != 0) : result != 0) return 7;
        if (cancel || fail) {
            if ([NSFileManager.defaultManager fileExistsAtPath:recording.movieURL.path] ||
                [NSFileManager.defaultManager fileExistsAtPath:recording.segmentURL.path]) return 8;
        } else {
            NSString *playlist = [NSString stringWithContentsOfURL:
                [recording.segmentURL URLByAppendingPathComponent:@"index.m3u8"]
                encoding:NSUTF8StringEncoding error:&error];
            if (![playlist containsString:@"#EXT-X-ENDLIST"] || recording.segmentCount < 2) return 9;
        }
        return 0;
    }
}
