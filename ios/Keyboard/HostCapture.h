#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

/// Which app the keyboard is typing into, so the app can send the user back
/// there after arming the microphone.
///
/// iOS 26.4 removed every way to ask. This reads the bundle id off a private
/// UIKit callback instead, so it can stop working in any iOS update and would
/// not pass App Review. A nil host only means the user swipes back by hand.
/// Adapted from KeyboardHostBundleID, see THIRD_PARTY.md.
@interface HostCapture : NSObject

@property(class, readonly, nullable) NSString *host;

/// Asks for a fresh callback. One may not come.
+ (void)refresh;

@end

NS_ASSUME_NONNULL_END
