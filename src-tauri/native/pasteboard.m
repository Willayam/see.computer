#import <AppKit/AppKit.h>

void *see_pasteboard_open(const char *name) {
    @autoreleasepool {
        NSPasteboard *board = name
            ? [NSPasteboard pasteboardWithName:[NSString stringWithUTF8String:name]]
            : NSPasteboard.generalPasteboard;
        return (__bridge_retained void *)board;
    }
}

void see_pasteboard_release(void *value) {
    CFRelease(value);
}

intptr_t see_pasteboard_count(void *handle) {
    return [(__bridge NSPasteboard *)handle changeCount];
}

void *see_pasteboard_snapshot(void *handle, intptr_t expected) {
    @autoreleasepool {
        NSPasteboard *board = (__bridge NSPasteboard *)handle;
        if (board.changeCount != expected) return NULL;
        NSMutableArray *copies = [NSMutableArray array];
        NSArray<NSPasteboardItem *> *items = board.pasteboardItems;
        if (!items.count && board.types.count) return NULL;
        for (NSPasteboardItem *item in items) {
            NSPasteboardItem *copy = [NSPasteboardItem new];
            for (NSPasteboardType type in item.types) {
                NSData *data = [item dataForType:type];
                if (!data || ![copy setData:data forType:type]) return NULL;
            }
            [copies addObject:copy];
        }
        return board.changeCount == expected ? (__bridge_retained void *)copies : NULL;
    }
}

// -1 means another process owns the pasteboard. -2 means the write failed.
intptr_t see_pasteboard_write(void *handle, intptr_t expected, const uint8_t *bytes, size_t length) {
    @autoreleasepool {
        NSPasteboard *board = (__bridge NSPasteboard *)handle;
        NSString *text = [[NSString alloc] initWithBytes:bytes length:length encoding:NSUTF8StringEncoding];
        if (!text) return -2;
        if (board.changeCount != expected) return -1;
        NSInteger owned = [board clearContents];
        if (![board setString:text forType:NSPasteboardTypeString]) return -2;
        return board.changeCount == owned ? owned : -1;
    }
}

bool see_pasteboard_restore(void *handle, intptr_t expected, void *snapshot) {
    @autoreleasepool {
        NSPasteboard *board = (__bridge NSPasteboard *)handle;
        NSArray *items = (__bridge NSArray *)snapshot;
        if (board.changeCount != expected) return false;
        [board clearContents];
        return items.count == 0 || [board writeObjects:items];
    }
}
