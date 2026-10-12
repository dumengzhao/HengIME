#import <Foundation/Foundation.h>
#import <Carbon/Carbon.h>

static NSString *prop(TISInputSourceRef s, CFStringRef key) {
    CFTypeRef v = TISGetInputSourceProperty(s, key);
    if (!v) return @"(nil)";
    if (CFGetTypeID(v) == CFBooleanGetTypeID()) {
        return CFBooleanGetValue((CFBooleanRef)v) ? @"YES" : @"NO";
    }
    if (CFGetTypeID(v) == CFStringGetTypeID()) {
        return (__bridge NSString *)v;
    }
    if (CFGetTypeID(v) == CFNumberGetTypeID()) {
        return [(__bridge NSNumber *)v stringValue];
    }
    if (CFGetTypeID(v) == CFArrayGetTypeID()) {
        return [(__bridge NSArray *)v componentsJoinedByString:@","];
    }
    return @"(other)";
}

static void dump_list(Boolean installed, const char *label) {
    CFArrayRef list = TISCreateInputSourceList(NULL, installed);
    if (!list) {
        printf("-- %s: TISCreateInputSourceList returned NULL --\n", label);
        return;
    }
    CFIndex n = CFArrayGetCount(list);
    printf("-- %s: %ld entries --\n", label, (long)n);
    for (CFIndex i = 0; i < n; i++) {
        TISInputSourceRef s = (TISInputSourceRef)CFArrayGetValueAtIndex(list, i);
        NSString *sid = prop(s, kTISPropertyInputSourceID);
        NSString *bid = prop(s, kTISPropertyBundleID);
        if ([sid hasPrefix:@"com.apple."]) continue;
        printf("   ID=%-46s bundle=%-34s enabled=%-3s selectable=%-3s cat=%s\n",
               [sid UTF8String],
               [bid UTF8String],
               [prop(s, kTISPropertyInputSourceIsEnabled) UTF8String],
               [prop(s, kTISPropertyInputSourceIsSelectCapable) UTF8String],
               [prop(s, kTISPropertyInputSourceCategory) UTF8String]);
    }
    CFRelease(list);
}

static void lookup(const char *sid) {
    NSString *want = [NSString stringWithUTF8String:sid];
    CFArrayRef list = TISCreateInputSourceList(NULL, false);
    CFIndex n = list ? CFArrayGetCount(list) : 0;
    BOOL found = NO;
    for (CFIndex i = 0; i < n; i++) {
        TISInputSourceRef s = (TISInputSourceRef)CFArrayGetValueAtIndex(list, i);
        if ([prop(s, kTISPropertyInputSourceID) isEqualToString:want]) { found = YES; break; }
    }
    if (list) CFRelease(list);
    TISInputSourceRef src = TISCopyInputSourceForLanguage(CFSTR("en"));
    if (src) CFRelease(src);
    printf("lookup %-44s in ENABLED list -> %s\n", sid, found ? "FOUND" : "NOT FOUND");
}

int main(int argc, char **argv) {
    @autoreleasepool {
        printf("===== 非 Apple 且已安装的输入源 =====\n");
        dump_list(true, "installed(non-Apple)");

        printf("\n===== ENABLED 列表中的非 Apple 项 =====\n");
        dump_list(false, "enabled(non-Apple)");

        printf("\n===== 定向查询 =====\n");
        if (argc > 1) {
            for (int i = 1; i < argc; i++) lookup(argv[i]);
        } else {
            lookup("im.rime.inputmethod.Squirrel");
            lookup("im.heng.inputmethod.Squirrel.dev");
            lookup("im.heng.inputmethod.Squirrel.dev.Hans");
            lookup("com.tencent.inputmethod.wetype");
        }
    }
    return 0;
}
