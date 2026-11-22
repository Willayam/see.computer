#import "SwipeRuntime.h"
#import <ExecuTorch/ExecuTorch.h>

@implementation SwipeEncoder {
    ExecuTorchModule *_module;
}
- (instancetype)initWithPath:(NSString *)path error:(NSError **)error {
    if ((self = [super init])) {
        _module = [[ExecuTorchModule alloc] initWithFilePath:path];
        if (![_module loadMethod:@"forward" error:error]) return nil;
    }
    return self;
}
- (NSData *)emissions:(const float *)features keys:(const float *)keys mask:(const uint8_t *)mask {
    NSArray *inputs = @[
        [[ExecuTorchTensor alloc] initWithBytes:features shape:@[@1,@2,@64] dataType:ExecuTorchDataTypeFloat],
        [[ExecuTorchTensor alloc] initWithBytes:keys shape:@[@1,@64,@2] dataType:ExecuTorchDataTypeFloat],
        [[ExecuTorchTensor alloc] initWithBytes:mask shape:@[@1,@64] dataType:ExecuTorchDataTypeBool]
    ];
    ExecuTorchTensor *tensor = [_module forwardWithTensors:inputs error:nil].firstObject.tensorValue;
    __block NSData *data = nil;
    [tensor bytesWithHandler:^(const void *bytes, NSInteger count, ExecuTorchDataType type) {
        if (count == 32 * 65 && type == ExecuTorchDataTypeFloat)
            data = [NSData dataWithBytes:bytes length:count * sizeof(float)];
    }];
    return data;
}
@end
