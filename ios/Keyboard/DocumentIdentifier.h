#import <UIKit/UIInputViewController.h>

// UIKit can return nil during extension attachment, despite declaring the UUID nonnull.
static inline NSUUID * _Nullable SeeDocumentIdentifier(id<UITextDocumentProxy> _Nullable proxy) {
    return proxy.documentIdentifier;
}
