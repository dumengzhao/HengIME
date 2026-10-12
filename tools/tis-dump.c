// 打印输入源的全部 TIS 属性，用于和正常输入法逐项对比
// 编译: clang -x objective-c -o /tmp/heng-tisdump tis-dump.c -framework Carbon -framework Foundation
#import <Carbon/Carbon.h>
#import <Foundation/Foundation.h>

// 在 tbd 里导出但没进公开头文件，手动声明
// 在 tbd 里导出但没进公开头文件，手动声明
extern CFStringRef kTISPropertyInputSourceIsInvisible;
extern CFStringRef kTISPropertyInputSourceIsFromSystem;
extern CFStringRef kTISPropertyIntendedLanguage;
extern CFStringRef kTISPropertyScriptCode;

static void dumpOne(TISInputSourceRef src, BOOL inEnabled) {
    CFStringRef sid = TISGetInputSourceProperty(src, kTISPropertyInputSourceID);
    CFStringRef mid = kTISPropertyInputModeID ? TISGetInputSourceProperty(src, kTISPropertyInputModeID) : NULL;
    printf("=== %s | mode=%s | %s ===\n",
           sid ? CFStringGetCStringPtr(sid, kCFStringEncodingUTF8) ?: "?" : "(nil)",
           mid ? CFStringGetCStringPtr(mid, kCFStringEncodingUTF8) ?: "?" : "-",
           inEnabled ? "在ENABLED列表" : "只在INSTALLED列表");

    struct { CFStringRef key; const char *name; } keys[] = {
        { kTISPropertyInputSourceID,        "InputSourceID" },
        { kTISPropertyBundleID,             "BundleID" },
        { kTISPropertyLocalizedName,        "LocalizedName" },
        { kTISPropertyInputSourceLanguages, "Languages" },
        { kTISPropertyInputSourceCategory,  "Category" },
        { kTISPropertyInputSourceType,      "Type" },
        { kTISPropertyIconImageURL,         "IconImageURL" },
        { kTISPropertyInputSourceIsASCIICapable,   "IsASCIICapable" },
        { kTISPropertyInputSourceIsEnableCapable,  "IsEnableCapable" },
        { kTISPropertyInputSourceIsSelectCapable,  "IsSelectCapable" },
        { kTISPropertyInputSourceIsEnabled,        "IsEnabled" },
        { kTISPropertyInputSourceIsSelected,       "IsSelected" },
        { kTISPropertyInputSourceIsInvisible,      "IsInvisible" },
        { kTISPropertyInputSourceIsFromSystem,     "IsFromSystem" },
        { kTISPropertyIntendedLanguage,     "IntendedLanguage" },
        { kTISPropertyScriptCode,           "ScriptCode" },
    };
    for (unsigned long i = 0; i < sizeof(keys)/sizeof(keys[0]); i++) {
        CFTypeRef v = TISGetInputSourceProperty(src, keys[i].key);
        if (!v) { printf("  %-20s = <absent>\n", keys[i].name); continue; }
        CFStringRef s = CFStringCreateWithFormat(NULL, NULL, CFSTR("%@"), v);
        char buf[1024];
        if (CFStringGetCString(s, buf, sizeof(buf), kCFStringEncodingUTF8))
            printf("  %-20s = %s\n", keys[i].name, buf);
        else
            printf("  %-20s = <type %s>\n", keys[i].name,
                   CFGetTypeID(v) == CFBooleanGetTypeID() ?
                   (CFBooleanGetValue((CFBooleanRef)v) ? "true" : "false") : "opaque");
        CFRelease(s);
    }
    printf("\n");
}

int main(void) {
    // ENABLED 列表 = 系统设置「输入法」面板真正渲染的来源
    CFArrayRef en = TISCreateInputSourceList(NULL, FALSE);
    // INSTALLED 列表 = 全部已安装
    CFArrayRef all = TISCreateInputSourceList(NULL, TRUE);

    printf("######## ENABLED 列表（系统设置实际显示的）########\n");
    for (CFIndex i = 0; en && i < CFArrayGetCount(en); i++)
        dumpOne((TISInputSourceRef)CFArrayGetValueAtIndex(en, i), YES);

    printf("\n######## INSTALLED 里含 heng / rime / wetype 的条目 ########\n");
    for (CFIndex i = 0; all && i < CFArrayGetCount(all); i++) {
        TISInputSourceRef s = (TISInputSourceRef)CFArrayGetValueAtIndex(all, i);
        CFStringRef sid = TISGetInputSourceProperty(s, kTISPropertyInputSourceID);
        if (!sid) continue;
        NSString *n = (__bridge NSString *)sid;
        if ([n containsString:@"heng"] || [n containsString:@"rime"] || [n containsString:@"wetype"]
            || [n containsString:@"SCIM"])
            dumpOne(s, NO);
    }
    if (en) CFRelease(en);
    if (all) CFRelease(all);
    return 0;
}
