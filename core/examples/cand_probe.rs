// 离屏渲染候选窗并导出 PPM —— 布局排查专用。
// 复现真实输入态（字母行 + 单行候选条），出图后用工具量格子实际落点，
// 避免靠公式反推被scale/舍入带偏。
fn main() {
    // 与真实输入「ni hao」一致：8 个候选 + 1 个高亮
    let texts = [
        "你好", "拟好", "你", "尼", "泥", "逆", "拟", "腻", "倪",
    ];
    // 逻辑坐标交给 core 内部 measure_w 之外的粗略估算：
    // 这里只关心纵向落点，x 用近似值即可（宽度误差不影响 y 结论）。
    let scale = 2.0f32;
    let font = 16i32;
    let k = font as f32 / 14.0;
    let margin = (6.0 * k).round() as i32;
    let pad_l = (6.0 * k).round() as i32;
    let gap = (3.0 * k).round() as i32;
    let nw = (11.0 * k / 2.0).round() as i32; // 序号「1」约半字宽
    let flow_top = (2.0 * k).round() as i32;

    let mut cells: Vec<(i32, i32, i32, bool, String)> = Vec::new();
    let mut x = margin;
    let right = 460 - margin - (58.0 * k).round() as i32;
    for (i, t) in texts.iter().enumerate() {
        let text_w = (t.chars().count() as f32 * font as f32).round() as i32;
        let w = pad_l + nw + gap + text_w + pad_l;
        if x + w > right && !cells.is_empty() {
            break;
        }
        cells.push((x, flow_top, w, i == 0, t.to_string()));
        x += w + gap;
    }
    let panel_width = 460;
    let ok = heng_core::ui::debug_render_cand(
        "ni hao",
        &cells,
        panel_width,
        font,
        scale,
        "candprobe",
    );
    if !ok {
        std::process::exit(1);
    }
}
