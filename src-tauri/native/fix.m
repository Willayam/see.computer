#import <AppKit/AppKit.h>
#include <stdlib.h>
#include <string.h>

// A wrapping text field rather than a text view: Return has to press Save,
// and a text view would take it as a newline.
char *see_fix_prompt(const char *text) {
    [NSApp activateIgnoringOtherApps:YES];
    NSAlert *alert = [[NSAlert alloc] init];
    alert.messageText = @"Fix the last dictation";
    alert.informativeText = @"Change the words to what you said. see.computer learns the spellings you fix.";
    [alert addButtonWithTitle:@"Save"];
    [alert addButtonWithTitle:@"Cancel"];

    NSTextField *editor = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 0, 380, 96)];
    editor.stringValue = text ? @(text) : @"";
    editor.usesSingleLineMode = NO;
    editor.cell.wraps = YES;
    editor.cell.scrollable = NO;
    editor.lineBreakMode = NSLineBreakByWordWrapping;
    alert.accessoryView = editor;
    [alert layout];
    alert.window.initialFirstResponder = editor;

    if ([alert runModal] != NSAlertFirstButtonReturn) return NULL;
    return strdup(editor.stringValue.UTF8String);
}
