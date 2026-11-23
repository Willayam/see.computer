#import <CoreGraphics/CoreGraphics.h>
#import <CoreText/CoreText.h>
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <Vision/Vision.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

static NSString *Recognize(CGImageRef image) {
    if (!image) return nil;
    VNRecognizeTextRequest *request = [VNRecognizeTextRequest new];
    request.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
    request.usesLanguageCorrection = NO;
    request.recognitionLanguages = @[@"en-US", @"sv-SE"];
    VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithCGImage:image options:@{}];
    if (![handler performRequests:@[request] error:nil]) return nil;

    NSArray *observations = [request.results sortedArrayUsingComparator:
        ^NSComparisonResult(VNRecognizedTextObservation *a, VNRecognizedTextObservation *b) {
            CGFloat ay = CGRectGetMidY(a.boundingBox), by = CGRectGetMidY(b.boundingBox);
            if (ay != by) return ay > by ? NSOrderedAscending : NSOrderedDescending;
            CGFloat ax = CGRectGetMinX(a.boundingBox), bx = CGRectGetMinX(b.boundingBox);
            if (ax == bx) return NSOrderedSame;
            return ax < bx ? NSOrderedAscending : NSOrderedDescending;
        }];
    NSMutableArray<NSString *> *lines = [NSMutableArray array];
    for (VNRecognizedTextObservation *observation in observations) {
        NSString *line = [observation topCandidates:1].firstObject.string;
        if (line.length) [lines addObject:line];
    }
    return [lines componentsJoinedByString:@"\n"];
}

static char *CopyJSON(NSArray *items) {
    if (!items) return NULL;
    NSData *data = [NSJSONSerialization dataWithJSONObject:items options:0 error:nil];
    NSString *json = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    return json ? strdup(json.UTF8String) : NULL;
}

char *see_ocr_file(const char *path) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithFileSystemRepresentation:path isDirectory:NO relativeToURL:nil];
        CGImageSourceRef source = CGImageSourceCreateWithURL((__bridge CFURLRef)url, NULL);
        if (!source) return NULL;
        CGImageRef image = CGImageSourceCreateImageAtIndex(source, 0, NULL);
        CFRelease(source);
        if (!image) return NULL;
        NSString *text = Recognize(image);
        CGImageRelease(image);
        return text ? strdup(text.UTF8String) : NULL;
    }
}

bool see_ocr_warm(void) {
    @autoreleasepool {
        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGContextRef context = CGBitmapContextCreate(NULL, 128, 64, 8, 0, space,
                                                     (CGBitmapInfo)kCGImageAlphaPremultipliedLast);
        CGColorSpaceRelease(space);
        if (!context) return false;
        CGContextSetRGBFillColor(context, 1, 1, 1, 1);
        CGContextFillRect(context, CGRectMake(0, 0, 128, 64));
        CTFontRef font = CTFontCreateWithName(CFSTR("Helvetica"), 24, NULL);
        NSAttributedString *label = [[NSAttributedString alloc] initWithString:@"Vision"
            attributes:@{(__bridge NSString *)kCTFontAttributeName: (__bridge id)font}];
        CFRelease(font);
        CTLineRef line = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)label);
        CGContextSetTextPosition(context, 8, 24);
        CTLineDraw(line, context);
        CFRelease(line);
        CGImageRef image = CGBitmapContextCreateImage(context);
        CGContextRelease(context);
        if (!image) return false;
        NSString *text = Recognize(image);
        CGImageRelease(image);
        return text != nil;
    }
}

char *see_window_titles(void) {
    @autoreleasepool {
        NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
        NSMutableArray *titles = [NSMutableArray array];
        for (NSDictionary *window in windows) {
            if ([window[(__bridge NSString *)kCGWindowLayer] integerValue] != 0) continue;
            NSString *owner = window[(__bridge NSString *)kCGWindowOwnerName] ?: @"";
            NSString *title = window[(__bridge NSString *)kCGWindowName] ?: @"";
            [titles addObject:@[owner, title]];
        }
        return CopyJSON(titles);
    }
}

static uint64_t MixBytes(uint64_t h, const uint8_t *bytes, size_t len) {
    size_t words = len / 8;
    for (size_t i = 0; i < words; i++) {
        uint64_t v;
        memcpy(&v, bytes + i * 8, sizeof(v));
        h ^= v;
        h *= 1099511628211ULL;
    }
    for (size_t i = words * 8; i < len; i++) {
        h ^= bytes[i];
        h *= 1099511628211ULL;
    }
    return h;
}

static bool ImageHash(CGImageRef image, uint64_t *outHash) {
    if (!image || !outHash) return false;
    CGDataProviderRef provider = CGImageGetDataProvider(image);
    if (!provider) return false;
    CFDataRef data = CGDataProviderCopyData(provider);
    if (!data) return false;
    const uint8_t *bytes = CFDataGetBytePtr(data);
    size_t total = (size_t)CFDataGetLength(data);
    size_t width = CGImageGetWidth(image);
    size_t height = CGImageGetHeight(image);
    size_t bpr = CGImageGetBytesPerRow(image);
    size_t bpp = CGImageGetBitsPerPixel(image);
    size_t rowBytes = (width * bpp + 7) / 8;
    if (rowBytes > bpr) rowBytes = bpr;

    uint64_t h = 14695981039346656037ULL;
    uint64_t meta[4] = {(uint64_t)width, (uint64_t)height, (uint64_t)bpp, (uint64_t)rowBytes};
    for (int i = 0; i < 4; i++) {
        h ^= meta[i];
        h *= 1099511628211ULL;
    }
    if (rowBytes == bpr) {
        size_t active = height * bpr;
        if (active > total) active = total;
        h = MixBytes(h, bytes, active);
    } else {
        for (size_t y = 0; y < height; y++) {
            size_t offset = y * bpr;
            if (offset + rowBytes > total) break;
            h = MixBytes(h, bytes + offset, rowBytes);
        }
    }
    CFRelease(data);
    *outHash = h;
    return true;
}

bool see_ocr_file_hash(const char *path, uint64_t *outHash) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithFileSystemRepresentation:path isDirectory:NO relativeToURL:nil];
        CGImageSourceRef source = CGImageSourceCreateWithURL((__bridge CFURLRef)url, NULL);
        if (!source) return false;
        CGImageRef image = CGImageSourceCreateImageAtIndex(source, 0, NULL);
        CFRelease(source);
        if (!image) return false;
        bool ok = ImageHash(image, outHash);
        CGImageRelease(image);
        return ok;
    }
}

static NSMutableDictionary<NSNumber *, NSArray *> *DisplayCache(void) {
    static NSMutableDictionary<NSNumber *, NSArray *> *cache;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ cache = [NSMutableDictionary new]; });
    return cache;
}

static void RecognizeDisplays(SCShareableContent *content, void (^complete)(NSArray *)) {
    dispatch_group_t group = dispatch_group_create();
    NSMutableArray *texts = [NSMutableArray array];
    NSMutableSet<NSNumber *> *liveIds = [NSMutableSet setWithCapacity:content.displays.count];
    for (SCDisplay *display in content.displays) {
        [texts addObject:[NSNull null]];
        [liveIds addObject:@(display.displayID)];
    }
    NSMutableDictionary<NSNumber *, NSArray *> *cache = DisplayCache();
    @synchronized (cache) {
        for (NSNumber *key in cache.allKeys) {
            if (![liveIds containsObject:key]) [cache removeObjectForKey:key];
        }
    }
    dispatch_queue_t queue = dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0);
    [content.displays enumerateObjectsUsingBlock:^(SCDisplay *display, NSUInteger index, BOOL *stop __unused) {
        NSNumber *displayKey = @(display.displayID);
        SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
        SCStreamConfiguration *config = [SCStreamConfiguration new];
        config.width = lround(display.width * filter.pointPixelScale);
        config.height = lround(display.height * filter.pointPixelScale);
        config.showsCursor = NO;
        dispatch_group_enter(group);
        [SCScreenshotManager captureImageWithFilter:filter configuration:config
                                  completionHandler:^(CGImageRef image, NSError *error) {
            if (error || !image) {
                dispatch_group_leave(group);
                return;
            }
            CGImageRetain(image);
            dispatch_group_async(group, queue, ^{
                @autoreleasepool {
                    uint64_t hash = 0;
                    bool hashed = ImageHash(image, &hash);
                    NSString *text = nil;
                    if (hashed) {
                        @synchronized (cache) {
                            NSArray *entry = cache[displayKey];
                            if (entry && [entry[0] unsignedLongLongValue] == hash) {
                                text = entry[1];
                            }
                        }
                    }
                    if (!text) {
                        text = Recognize(image);
                        if (text && hashed) {
                            @synchronized (cache) {
                                cache[displayKey] = @[@(hash), text];
                            }
                        }
                    }
                    CGImageRelease(image);
                    @synchronized (texts) {
                        if (text) texts[index] = text;
                    }
                }
            });
            dispatch_group_leave(group);
        }];
    }];
    dispatch_group_notify(group, queue, ^{
        // One sleeping or failed display shouldn't hide the text on the others.
        [texts removeObject:[NSNull null]];
        complete(texts);
    });
}

char *see_ocr_displays(uint32_t timeoutMs) {
    static atomic_bool busy = false;
    @autoreleasepool {
        if (!CGPreflightScreenCaptureAccess() || atomic_exchange(&busy, true)) return NULL;
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        __block NSArray *texts = nil;
        void (^complete)(NSArray *) = ^(NSArray *result) {
            texts = result;
            atomic_store(&busy, false);
            dispatch_semaphore_signal(done);
        };
        [SCShareableContent getShareableContentExcludingDesktopWindows:NO
                                                 onScreenWindowsOnly:YES
                                                   completionHandler:^(SCShareableContent *content, NSError *error) {
            if (error || !content) {
                complete(nil);
                return;
            }
            RecognizeDisplays(content, complete);
        }];
        if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, (int64_t)timeoutMs * NSEC_PER_MSEC))) {
            return NULL;
        }
        return CopyJSON(texts);
    }
}
