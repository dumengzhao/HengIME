// 尝试用 TIS API 把输入源真正激活到当前会话
// clang -x objective-c -o /tmp/heng-tisenable tis-enable.c -framework Carbon -framework Foundation
#import <Carbon/Carbon.h>
#import <Foundation/Foundation.h>

int main(int argc, char **argv) {
    if (argc < 2) { printf("usage: tis-enable <InputSourceID>\n"); return 2; }
    NSString *want = @(argv[1]);

    // 1. 从「全部已安装」里找
    NSDictionary *flt = @{ (NSString *)kTISPropertyInputSourceID : want };
    CFArrayRef list = TISCreateInputSourceList((__bridge CFDictionaryRef)flt, TRUE);
    if (!list || CFArrayGetCount(list) == 0) {
        // Input Mode 要用 InputModeID 过滤
        NSDictionary *flt2 = @{ (NSString *)kTISPropertyInputModeID : want };
        list = TISCreateInputSourceList((__bridge CFDictionaryRef)flt2, TRUE);
    }
    if (!list || CFArrayGetCount(list) == 0) {
        printf("FAIL: 找不到输入源 %s\n", want.UTF8String);
        return 1;
    }
    TISInputSourceRef src = (TISInputSourceRef)CFArrayGetValueAtIndex(list, 0);

    OSStatus e1 = TISEnableInputSource(src);
    printf("TISEnableInputSource  -> %d\n", (int)e1);
    OSStatus e2 = TISSelectInputSource(src);
    printf("TISSelectInputSource  -> %d\n", (int)e2);

    // 2. 复查 ENABLED 列表
    CFArrayRef en = TISCreateInputSourceList(NULL, FALSE);
    BOOL found = NO;
    for (CFIndex i = 0; en && i < CFArrayGetCount(en); i++) {
        TISInputSourceRef s = (TISInputSourceRef)CFArrayGetValueAtIndex(en, i);
        CFStringRef sid = TISGetInputSourceProperty(s, kTISPropertyInputSourceID);
        CFStringRef mid = TISGetInputSourceProperty(s, kTISPropertyInputModeID);
        NSString *a = sid ? (__bridge NSString *)sid : @"";
        NSString *b = mid ? (__bridge NSString *)mid : @"";
        if ([a isEqualToString:want] || [b isEqualToString:want]) found = YES;
    }
    printf("复查 ENABLED 列表     -> %s\n", found ? "已包含 ✓" : "仍未包含 ✗");
    if (en) CFRelease(en);
    if (list) CFRelease(list);
    return found ? 0 : 1;
}
