#import "HostCapture.h"

#if STORE

// TestFlight builds use no private calls, so the host stays unknown.
@implementation HostCapture

+ (NSString *)host {
    return nil;
}

+ (void)refresh {
}

@end

#else

#import <objc/message.h>
#import <objc/runtime.h>
#import <os/lock.h>

static os_unfair_lock lock = OS_UNFAIR_LOCK_INIT;
static NSString *captured = nil;
static IMP original = NULL;

static NSString *Acceptable(id value) {
    if (![value isKindOfClass:NSString.class]) return nil;
    NSString *bundle = value;
    if (![bundle containsString:@"."]) return nil;
    if ([bundle hasPrefix:@"computer.see.ios"]) return nil;
    if ([bundle isEqualToString:@"com.apple.springboard"]) return nil;
    return bundle;
}

// The bundle id is on the `change` argument, not on the receiver.
static void KeyboardChanged(id self, SEL _cmd, id change, id completion) {
    @try {
        NSString *bundle = Acceptable([change valueForKey:@"_sourceBundleIdentifier"]);
        os_unfair_lock_lock(&lock);
        captured = bundle;
        os_unfair_lock_unlock(&lock);
    } @catch (__unused NSException *exception) {
    }
    if (original) ((void (*)(id, SEL, id, id))original)(self, _cmd, change, completion);
}

// iOS gates the callback on this class method.
static BOOL AlwaysEnabled(__unused id self, __unused SEL _cmd) {
    return YES;
}

@implementation HostCapture

// +load, because the first callback carries the host and arrives before Swift starts.
+ (void)load {
    Method enabled = class_getClassMethod(NSClassFromString(@"_UIKeyboardArbiterClient"),
                                          NSSelectorFromString(@"enabled"));
    if (enabled) method_setImplementation(enabled, (IMP)AlwaysEnabled);

    Method changed = class_getInstanceMethod(NSClassFromString(@"_UIKeyboardArbiterClientInputDestination"),
                                             NSSelectorFromString(@"queue_keyboardChanged:onComplete:"));
    if (changed) {
        original = method_getImplementation(changed);
        method_setImplementation(changed, (IMP)KeyboardChanged);
    }
}

+ (NSString *)host {
    os_unfair_lock_lock(&lock);
    NSString *host = captured;
    os_unfair_lock_unlock(&lock);
    return host;
}

+ (void)refresh {
    Class client = NSClassFromString(@"_UIKeyboardArbiterClient");
    SEL shared = NSSelectorFromString(@"automaticSharedArbiterClient");
    SEL check = NSSelectorFromString(@"checkConnection");
    if (![client respondsToSelector:shared]) return;
    id instance = ((id (*)(id, SEL))objc_msgSend)(client, shared);
    if ([instance respondsToSelector:check]) ((void (*)(id, SEL))objc_msgSend)(instance, check);
}

@end

#endif
