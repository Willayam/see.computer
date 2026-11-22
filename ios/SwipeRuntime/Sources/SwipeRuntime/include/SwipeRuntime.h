#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN
@interface SwipeEncoder : NSObject
- (nullable instancetype)initWithPath:(NSString *)path error:(NSError **)error;
- (nullable NSData *)emissions:(const float *)features keys:(const float *)keys mask:(const uint8_t *)mask;
@end
NS_ASSUME_NONNULL_END
