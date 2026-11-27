#import "../src-tauri/native/pasteboard.m"
#include <assert.h>

int main(void) {
    @autoreleasepool {
        NSString *name = [@"see-pasteboard-test-" stringByAppendingString:NSUUID.UUID.UUIDString];
        void *handle = see_pasteboard_open(name.UTF8String);
        NSPasteboard *board = (__bridge NSPasteboard *)handle;
        NSPasteboardItem *rich = [NSPasteboardItem new];
        NSData *format = [@"{\\rtf1 example}" dataUsingEncoding:NSUTF8StringEncoding];
        [rich setData:format forType:NSPasteboardTypeRTF];
        [rich setString:@"example" forType:NSPasteboardTypeString];
        NSPasteboardItem *file = [NSPasteboardItem new];
        [file setString:@"file:///fixture.png" forType:NSPasteboardTypeFileURL];
        [board clearContents];
        assert(([board writeObjects:@[rich, file]]));
        void *snapshot = see_pasteboard_snapshot(handle, board.changeCount);
        assert(snapshot);
        intptr_t first = see_pasteboard_write(handle, board.changeCount, (const uint8_t *)"yes", 3);
        assert(first >= 0);
        intptr_t second = see_pasteboard_write(handle, first, (const uint8_t *)"yes", 3);
        assert(second > first);
        assert(see_pasteboard_restore(handle, second, snapshot));
        assert(board.pasteboardItems.count == 2);
        assert([[board.pasteboardItems[0] dataForType:NSPasteboardTypeRTF] isEqual:format]);
        assert([[board.pasteboardItems[0] stringForType:NSPasteboardTypeString] isEqual:@"example"]);
        assert([[board.pasteboardItems[1] stringForType:NSPasteboardTypeFileURL] isEqual:@"file:///fixture.png"]);
        intptr_t owned = see_pasteboard_write(handle, board.changeCount, (const uint8_t *)"words", 5);
        assert(owned >= 0);
        [board clearContents];
        [board setString:@"another copy" forType:NSPasteboardTypeString];
        assert(!see_pasteboard_restore(handle, owned, snapshot));
        assert(see_pasteboard_write(handle, owned, (const uint8_t *)"words", 5) == -1);
        assert([[board stringForType:NSPasteboardTypeString] isEqual:@"another copy"]);
        see_pasteboard_release(snapshot);
        [board clearContents];
        snapshot = see_pasteboard_snapshot(handle, board.changeCount);
        assert(snapshot);
        owned = see_pasteboard_write(handle, board.changeCount, (const uint8_t *)"last", 4);
        assert(see_pasteboard_restore(handle, owned, snapshot));
        assert(board.pasteboardItems.count == 0);
        see_pasteboard_release(snapshot);
        [board releaseGlobally];
        see_pasteboard_release(handle);
    }
    return 0;
}
