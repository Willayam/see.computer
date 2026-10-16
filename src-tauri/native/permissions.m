// Microphone consent and app activation, which need AVFoundation blocks and
// AppKit rather than anything Rust reaches easily.

#import <AVFoundation/AVFoundation.h>
#import <AppKit/AppKit.h>

// AVAuthorizationStatus: 0 not determined, 1 restricted, 2 denied, 3 authorized.
int see_microphone_status(void) {
    return (int)[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
}

// Shows the system prompt only while the status is not determined.
void see_microphone_request(void) {
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio
                             completionHandler:^(BOOL granted) {
                                 (void)granted;
                             }];
}

// A menu-bar app is never frontmost on its own, so a new window would open
// behind whatever the user was doing.
void see_activate_app(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSApp activate];
    });
}
