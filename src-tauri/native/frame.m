// Frame extraction from a finished recording, called from clip.rs.
//
// The sync AVFoundation APIs are deprecated in favor of async loading, but the
// callers here already run on a worker thread, so blocking is the simple truth.
#import <AVFoundation/AVFoundation.h>
#import <ImageIO/ImageIO.h>

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

double sc_clip_duration_seconds(const char *path) {
    @autoreleasepool {
        NSString *file = [NSString stringWithUTF8String:path];
        AVURLAsset *asset = [AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:file] options:nil];
        CMTime duration = asset.duration;
        if (CMTIME_IS_INVALID(duration) || CMTIME_IS_INDEFINITE(duration)) return -1.0;
        double seconds = CMTimeGetSeconds(duration);
        return seconds >= 0 ? seconds : -1.0;
    }
}

int sc_frame_jpeg(
    const char *path,
    long long at_ms,
    int max_width,
    int tolerance_ms,
    const char *out_path
) {
    @autoreleasepool {
        NSString *file = [NSString stringWithUTF8String:path];
        AVURLAsset *asset = [AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:file] options:nil];
        if ([asset tracksWithMediaType:AVMediaTypeVideo].count == 0) return 1;
        AVAssetImageGenerator *generator = [[AVAssetImageGenerator alloc] initWithAsset:asset];
        generator.appliesPreferredTrackTransform = YES;
        generator.maximumSize = CGSizeMake(max_width, 0);
        generator.requestedTimeToleranceBefore = CMTimeMake(tolerance_ms, 1000);
        generator.requestedTimeToleranceAfter = CMTimeMake(tolerance_ms, 1000);
        NSError *error = nil;
        CGImageRef image = [generator copyCGImageAtTime:CMTimeMake(at_ms, 1000)
                                             actualTime:NULL
                                                  error:&error];
        if (image == NULL) return 2;
        NSURL *out = [NSURL fileURLWithPath:[NSString stringWithUTF8String:out_path]];
        CGImageDestinationRef sink =
            CGImageDestinationCreateWithURL((__bridge CFURLRef)out, CFSTR("public.jpeg"), 1, NULL);
        if (sink == NULL) {
            CGImageRelease(image);
            return 3;
        }
        NSDictionary *options =
            @{(__bridge NSString *)kCGImageDestinationLossyCompressionQuality : @0.92};
        CGImageDestinationAddImage(sink, image, (__bridge CFDictionaryRef)options);
        bool finished = CGImageDestinationFinalize(sink);
        CFRelease(sink);
        CGImageRelease(image);
        return finished ? 0 : 4;
    }
}

#pragma clang diagnostic pop
