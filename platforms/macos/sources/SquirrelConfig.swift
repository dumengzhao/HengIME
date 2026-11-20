//
//  SquirrelConfig.swift
//  Squirrel
//
//  Created by Leo Liu on 5/9/24.
//
//  HengIME 换血：配置读取经 heng_config_* C ABI（librime RimeConfig 直通）。
//

import AppKit

final class SquirrelConfig {
  private(set) var isOpen = false

  private var cache: [String: Any] = [:]
  private var config: UnsafeMutableRawPointer?
  private var baseConfig: SquirrelConfig?

  func openBaseConfig() -> Bool {
    close()
    config = heng_config_open("squirrel")
    isOpen = config != nil
    return isOpen
  }

  func open(schemaID: String, baseConfig: SquirrelConfig?) -> Bool {
    close()
    config = schemaID.withCString { heng_config_open_schema($0) }
    if config != nil {
      isOpen = true
      self.baseConfig = baseConfig
    }
    return isOpen
  }

  func close() {
    if isOpen {
      if let config = config {
        heng_config_close(config)
      }
      config = nil
      baseConfig = nil
      isOpen = false
    }
  }

  deinit {
    close()
  }

  func has(section: String) -> Bool {
    guard isOpen, let config = config else { return false }
    var iterator = HengConfigIterator()
    let hit = section.withCString { heng_config_begin_map(config, $0, &iterator) } != 0
    if hit {
      heng_config_end(&iterator)
    }
    return hit
  }

  func getBool(_ option: String) -> Bool? {
    if let cachedValue = cachedValue(of: Bool.self, forKey: option) {
      return cachedValue
    }
    var value: Int32 = 0
    if isOpen, let config = config, option.withCString({ heng_config_get_bool(config, $0, &value) }) != 0 {
      cache[option] = value != 0
      return value != 0
    }
    return baseConfig?.getBool(option)
  }

  func getDouble(_ option: String) -> CGFloat? {
    if let cachedValue = cachedValue(of: Double.self, forKey: option) {
      return cachedValue
    }
    var value: Double = 0
    if isOpen, let config = config, option.withCString({ heng_config_get_double(config, $0, &value) }) != 0 {
      cache[option] = value
      return value
    }
    return baseConfig?.getDouble(option)
  }

  func getString(_ option: String) -> String? {
    if let cachedValue = cachedValue(of: String.self, forKey: option) {
      return cachedValue
    }
    if isOpen, let config = config, let value = readString(option, from: config) {
      cache[option] = value
      return value
    }
    return baseConfig?.getString(option)
  }

  func getColor(_ option: String, inSpace colorSpace: SquirrelTheme.RimeColorSpace) -> NSColor? {
    if let cachedValue = cachedValue(of: NSColor.self, forKey: option) {
      return cachedValue
    }
    if let colorStr = getString(option), let color = color(from: colorStr, inSpace: colorSpace) {
      cache[option] = color
      return color
    }
    return baseConfig?.getColor(option, inSpace: colorSpace)
  }

  func getAppOptions(_ appName: String) -> [String: Bool] {
    var appOptions = [String: Bool]()
    guard isOpen, let config = config else { return appOptions }
    let rootKey = "app_options/\(appName)"
    var iterator = HengConfigIterator()
    let began = rootKey.withCString { heng_config_begin_map(config, $0, &iterator) } != 0
    if began {
      while heng_config_next(&iterator) != 0 {
        if let key = iterator.key, let path = iterator.path, let value = getBool(String(cString: path)) {
          appOptions[String(cString: key)] = value
        }
      }
      heng_config_end(&iterator)
    }
    return appOptions
  }
}

private extension SquirrelConfig {
  func cachedValue<T>(of: T.Type, forKey key: String) -> T? {
    return cache[key] as? T
  }

  /// heng_config_get_string 读取：先固定缓冲，缓冲不足时按返回的所需长度重试。
  /// （ABI 约定 buf_len<=0 直接返回未命中，不能拿 0 去探测长度）
  func readString(_ option: String, from config: UnsafeMutableRawPointer) -> String? {
    option.withCString { key -> String? in
      var size: Int32 = 256
      var buffer = [CChar](repeating: 0, count: Int(size))
      var written = heng_config_get_string(config, key, &buffer, size)
      if written > size {
        // 键存在但缓冲不足：返回值 = 所需长度（含 NUL），按它重试一次
        size = written
        buffer = [CChar](repeating: 0, count: Int(size))
        written = heng_config_get_string(config, key, &buffer, size)
      }
      guard written > 0, written < size else { return nil }
      return String(cString: buffer)
    }
  }

  func color(from colorStr: String, inSpace colorSpace: SquirrelTheme.RimeColorSpace) -> NSColor? {
    if let matched = try? /0x([A-Fa-f0-9]{2})([A-Fa-f0-9]{2})([A-Fa-f0-9]{2})([A-Fa-f0-9]{2})/.wholeMatch(in: colorStr) {
      let (_, alpha, blue, green, red) = matched.output
      return color(alpha: Int(alpha, radix: 16)!, red: Int(red, radix: 16)!, green: Int(green, radix: 16)!, blue: Int(blue, radix: 16)!, colorSpace: colorSpace)
    } else if let matched = try? /0x([A-Fa-f0-9]{2})([A-Fa-f0-9]{2})([A-Fa-f0-9]{2})/.wholeMatch(in: colorStr) {
      let (_, blue, green, red) = matched.output
      return color(alpha: 255, red: Int(red, radix: 16)!, green: Int(green, radix: 16)!, blue: Int(blue, radix: 16)!, colorSpace: colorSpace)
    } else {
      return nil
    }
  }

  func color(alpha: Int, red: Int, green: Int, blue: Int, colorSpace: SquirrelTheme.RimeColorSpace) -> NSColor {
    switch colorSpace {
    case .displayP3:
      return NSColor(displayP3Red: CGFloat(red) / 255,
                     green: CGFloat(green) / 255,
                     blue: CGFloat(blue) / 255,
                     alpha: CGFloat(alpha) / 255)
    case .sRGB:
      return NSColor(srgbRed: CGFloat(red) / 255,
                     green: CGFloat(green) / 255,
                     blue: CGFloat(blue) / 255,
                     alpha: CGFloat(alpha) / 255)
    }
  }
}
