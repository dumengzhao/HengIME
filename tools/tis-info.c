#import <Foundation/Foundation.h>
#import <Carbon/Carbon.h>

static NSString *p(TISInputSourceRef s, CFStringRef k) {
    CFTypeRef v = TISGetInputSourceProperty(s, k);
    if (!v) return @"(nil)";
    if (CFGetTypeID(v) == CFStringGetTypeID()) return (__bridge NSString *)v;
    if (CFGetTypeID(v) == CFURLGetTypeID()) return [(__bridge NSURL *)v path];
    if (CFGetTypeID(v) == CFBooleanGetTypeID()) return CFBooleanGetValue((CFBooleanRef)v) ? @"YES" : @"NO";
    return [NSString stringWithFormat:@"<%@>", v];
}

int main(int argc, char **argv) {
    @autoreleasepool {
        NSString *filter = argc > 1 ? [NSString stringWithUTF8String:argv[1]] : @"";
        CFArrayRef list = TISCreateInputSourceList(NULL, true);
        if (!list) { printf("NULL list\n"); return 1; }
        CFIndex n = CFArrayGetCount(list);
        for (CFIndex i = 0; i < n; i++) {
            TISInputSourceRef s = (TISInputSourceRef)CFArrayGetValueAtIndex(list, i);
            NSString *sid = p(s, kTISPropertyInputSourceID);
            if (filter.length && ![sid containsString:filter]) continue;
            printf("ID          = %s\n", [sid UTF8String]);
            printf("  bundleID  = %s\n", [p(s, kTISPropertyBundleID) UTF8String]);
            printf("  localized = %s\n", [p(s, kTISPropertyLocalizedName) UTF8String]);
            printf("  iconImage = %s\n", [p(s, kTISPropertyIconImageURL) UTF8String]);
            printf("  enabled   = %s   selectable = %s\n",
                   [p(s, kTISPropertyInputSourceIsEnabled) UTF8String],
                   [p(s, kTISPropertyInputSourceIsSelectCapable) UTF8String]);
            printf("\n");
        }
        CFRelease(list);
    }
    return 0;
}
