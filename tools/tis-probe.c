// TIS 探针：查询 macOS 输入源注册状态
//
// 编译：
//   clang -x objective-c -o /tmp/tisprobe tools/tis-probe.c \
//     -framework Carbon -framework Foundation
// 用法：
//   /tmp/tisprobe              列出所有含 squirrel/heng 的输入源 + 已启用列表
//   /tmp/tisprobe <子串>       额外按子串过滤
#import <Carbon/Carbon.h>
#import <Foundation/Foundation.h>

static void dumpOne(TISInputSourceRef s) {
    CFStringRef id = (CFStringRef)TISGetInputSourceProperty(s, kTISPropertyInputSourceID);
    CFStringRef bid = (CFStringRef)TISGetInputSourceProperty(s, kTISPropertyBundleID);
    CFBooleanRef en = (CFBooleanRef)TISGetInputSourceProperty(s, CFSTR("InputSourceIsEnabled"));
    CFBooleanRef sel = (CFBooleanRef)TISGetInputSourceProperty(s, CFSTR("InputSourceIsSelectCapable"));
    char ids[256] = {0}, bs[256] = {0};
    if (id) CFStringGetCString(id, ids, sizeof(ids), kCFStringEncodingUTF8);
    if (bid) CFStringGetCString(bid, bs, sizeof(bs), kCFStringEncodingUTF8);
    printf("  %-42s bundle=%-32s enabled=%s selectable=%s\n", ids, bs,
           en ? (CFBooleanGetValue(en) ? "Y" : "N") : "?",
           sel ? (CFBooleanGetValue(sel) ? "Y" : "N") : "?");
}

int main(int argc, char **argv) {
    @autoreleasepool {
        NSString *extra = argc > 1 ? [NSString stringWithUTF8String:argv[1]] : nil;

        CFArrayRef all = TISCreateInputSourceList(NULL, true);
        printf("installed = %ld\n", (long)CFArrayGetCount(all));
        printf("-- 匹配 squirrel / heng%s --\n", extra ? " / 额外子串" : "");
        int n = 0;
        for (CFIndex i = 0; all && i < CFArrayGetCount(all); i++) {
            TISInputSourceRef s = (TISInputSourceRef)CFArrayGetValueAtIndex(all, i);
            CFStringRef id = (CFStringRef)TISGetInputSourceProperty(s, kTISPropertyInputSourceID);
            if (!id) continue;
            NSString *sid = (__bridge NSString *)id;
            BOOL hit = [sid containsString:@"quirrel"] || [sid containsString:@"heng"];
            if (extra && [sid containsString:extra]) hit = YES;
            if (hit) { dumpOne(s); n++; }
        }
        printf("matches = %d\n", n);

        CFArrayRef en = TISCreateInputSourceList(NULL, false);
        printf("-- ENABLED (%ld) --\n", (long)CFArrayGetCount(en));
        for (CFIndex i = 0; en && i < CFArrayGetCount(en); i++) {
            dumpOne((TISInputSourceRef)CFArrayGetValueAtIndex(en, i));
        }
        if (all) CFRelease(all);
        if (en) CFRelease(en);
    }
    return 0;
}
