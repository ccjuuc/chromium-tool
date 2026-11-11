# 图像与 Chromium 图标转换

转换默认保留源图形的几何、颜色和透明度，不自动删除白色图形或背景。
输入文件及已有产物在转换失败时保留；单图转换先在独立暂存目录完成，再发布结果。

## SVG → Chromium `.icon`

所有 SVG 转换共用 usvg 解析入口，处理 CSS、引用、viewBox、变换、可见性和 SVG 默认 nonzero 填充规则。
路径以显式控制点输出，不依赖 Chromium 与 SVG 有差异的 shorthand 指令。
描边先按端点、连接、虚线规则展开为轮廓，再应用完整变换。填充与描边按 SVG paint-order 分别绘制。
非方形画布等比保留并居中放入方形画布，边长为原视口最长边向上取整，最大 16384。

默认严格转换支持普通实色路径和可表达为一个全局矩形的裁剪。
二值矢量蒙版先按绘制顺序计算覆盖区域，再由 Skia PathOps 与内容路径求交集，输出保留曲线与孔洞的填充路径。
亮度蒙版支持完全不透明的白色（保留）和黑色（移除）；alpha 蒙版支持任意完全不透明的实色。
处理遵循 nonzero/evenodd、描边展开、坐标变换、蒙版范围及嵌套裁剪，不按图标名称或形状识别，也不使用 CLEAR 擦除已绘制的背景。
组透明度先按从后到前的绘制顺序扣除不透明图层遮挡的区域，再对可见区域应用组透明度；不会把重叠处的 alpha 重复叠加，也不使用 CLEAR 擦除外部背景。
不重叠或被后续不透明图层遮挡的半透明内容也可直接转换；可见半透明图层的重叠仍需近似。
同色、同 alpha 和同抗锯齿设置的可见区域会合并。不同颜色共边的抗锯齿与 SVG 离屏合成可能有亚像素差异。

无位移的 `feOffset` 支持 `SourceGraphic` 和已解析的零位移结果引用链，保留滤镜区及每个有效输入的 primitive 子区域；未参与最终输出的分支不影响结果。
`SourceAlpha`、非零位移和其他有效滤镜操作仍需近似，不会当成空操作删除。
usvg 0.44 对显式 `primitiveUnits="objectBoundingBox"` 子区域的解析有已知限制，这种滤镜保守要求近似。
严格路径转换使用实际变换后的裁剪几何；resvg 0.44 的近似渲染对部分 primitive 子区域和旋转滤镜区存在限制，近似不保证纠正这些解析/渲染差异。

多个无法统一的独立裁剪区域、非矩形内容裁剪、图案、连续灰度/透明度蒙版、上述不支持的滤镜、混合模式和嵌入位图会明确标记为需要近似。
Chromium 的矩形裁剪使用硬边缘，因此与 SVG 裁剪在亚像素边缘上可能有渲染差异。
将二值蒙版转为路径交集保留填充几何；矢量边缘的抗锯齿与先渲染图层再乘蒙版仍可能有细微差异。

勾选“近似复杂效果”（API `allow_approximation: true`）后，上述效果通过 resvg 渲染为
128×128 采样结果，再量化 RGBA（步长 8）并输出合并的矩形路径。
产物注释记录近似原因和采样参数。此模式保留合成后的外观与透明度，但会损失细节，放大后有像素边缘，文件也可能较大。
选项默认关闭；不能将近似产物视为与任意尺寸源矢量完全等价。

“保留原图颜色”开启时写入固定 ARGB；关闭时生成供调用方指定颜色的模板图标，仍通过
`PATH_COLOR_ALPHA` 保留源图的半透明度。不透明模板路径省略颜色与 alpha 指令，完整继承调用方 ColorId 的 RGBA。
Chromium 的 `PATH_COLOR_ALPHA` **覆盖**调用方 alpha，并不与之相乘；例如源图 20% 透明度输出 51，而不是强制写 255。
近似模式中的部分覆盖采样也使用显式 alpha，无法通过 `.icon` 指令再乘任意运行时 alpha；需要整体淡出时应在调用方对完整图标应用图层透明度。
模板化会改变所有可见图形的颜色，包括源文件中的白色背景，不会自动去底，也不把透明孔洞改成白色图形。
模板模式支持所有 stop 透明度相同的线性渐变和焦点位于圆内的普通径向渐变：仅替换 RGB，保留轮廓、填充/描边透明度与 stop 透明度，不平均渐变颜色，不进行像素采样。
透明度渐变、退化变换和焦点处于边界/外部的径向渐变仍需近似。保留原图颜色时，彩色渐变仍需近似，因为 `.icon` 没有渐变着色器指令。
蒙版始终按原始颜色计算亮度，不受模板颜色替换规则影响。

文字须先转为路径。动态 SVG、foreignObject、外部图片/引用/样式导入和 data URL 嵌入的 SVG 明确不支持。
可将嵌入 SVG 合并进主文档，或改为嵌入 PNG/JPEG；嵌入位图转 `.icon` 需要近似模式。
解析基于 usvg 0.44 的静态 SVG 支持范围，不是完整浏览器 SVG 引擎。

## Chromium `.icon` → SVG / 预览

解析完整命令流，支持一行多个命令、注释和十六进制数，校验参数数量及有限数值。
图元加入当前路径，只有 `NEW_PATH` 开始新的绘制状态。默认 evenodd，保留描边、透明度、CLEAR 和全局 CLIP。
未知指令、依赖方向上下文的 `FLIPS_IN_RTL` 和依赖设备像素的 `STROKE, 0` 会明确报错。

多尺寸文件采用最后一个 representation（与 Chromium 默认图标尺寸一致），不会将多组图形混画。
静态 SVG 不会再按 Chromium 的 DIP/设备缩放比例动态选择 representation。
模板路径预览色为 `#424242`；实际 Chromium 颜色由调用方决定。

SVG 文件导出保留透明背景。白底/黑底只属于预览选项，不写入导出文件。
命令语义对照 Chromium 142 源码，并用独立 SVG 源图进行渲染回归；这些测试不替代真实 Chromium/Skia 的像素验证。

## ICO / ICNS / PNG

位图统一转换为 RGBA8 后编码，支持 RGB、灰度和 RGBA 输入，编码错误会返回失败。
ICO/ICNS 等比缩放并居中放入方形画布，非方形内容不会被拉伸。
ICNS 默认留白的内容边长约为画布的 208/256，适用于小尺寸源图。
ICNS 小尺寸沿用 Chromium 的 legacy RGB 加独立 alpha mask 表示，避免某些 macOS 加载器对 PNG 小图条目的兼容问题。
SVG → PNG 使用同一 SVG 解析入口，保留透明度；栅格画布上限为 6400 万像素。

## 本地服务与文件预览

服务默认监听 `127.0.0.1:3000`，校验 Host 与浏览器 Origin，不允许跨站请求。
文件预览仅限默认转换目录，以及通过本机目录选择器或显式批量转换登记的目录；登记在服务重启后失效。
仅提供图像/图标文件，校验规范路径的父目录，阻止通过符号链接读取目录外文件。
原始 SVG 响应附带 sandbox CSP，禁止脚本与外部资源加载。
`CHROMIUM_TOOL_BIND` 与 `CHROMIUM_TOOL_ALLOWED_HOSTS` 可供显式部署使用，但不提供身份认证。

转换 JSON 的 `format` 与 `output_path` 后缀必须一致，输出不得覆盖上传原图。
`emit_path_colors` 默认为 true，`allow_approximation` 默认为 false；批量转换使用相同选项。
转换错误使用独立代码：`requires_approximation` 表示超出直接矢量转换范围，`conversion_failed` 表示解析、校验、读写等失败；不会根据错误文案推断类别。
单图转换错误以 JSON `{code, message}` 返回；批量 `errors` 项包含 `{file, code, message}`。
批量请求可传 `files: ["example.svg"]` 仅转换指定目录中的单层 SVG 文件名；省略表示全部，空列表或非法选择会在转换前拒绝。
预览页的“仅近似重试 N 项”只重试 `requires_approximation` 项，不重跑成功项或改变严格模式开关。

## 验证

运行 `cargo test` 检查绘制语义、回转渲染、引用光芒、复杂效果选择、容器解码、颜色/透明度、失败保护及目录/来源校验。
运行 `cargo build` 构建本地服务。ICNS 文件的本地解码测试不能替代 macOS 系统实际加载验证。

路径布尔运算使用 `skia-safe 0.90` 的 Skia PathOps；支持的平台优先下载预编译 Skia，首次构建需要网络。没有匹配预编译包时，须按 rust-skia 文档配置原生 Skia 构建工具链。
这与 Chromium `.icon` 的运行时格式无关，生成图标不需要额外运行时依赖。

## Chromium 源码依据

- `ui/gfx/vector_icon_types.h` 与 `paint_vector_icon.cc`：`.icon` 没有 mask 指令；每个路径先采用调用方颜色，`PATH_COLOR_ALPHA` 覆盖 alpha，`PATH_COLOR_ARGB` 覆盖全部通道。
- `third_party/blink/renderer/core/paint/svg_mask_painter.cc` 与 `paint_property_tree_builder.cc`：SVG mask 按亮度/alpha图层以 `DstIn` 合成，不直接转为 `.icon`。
- `third_party/skia/include/pathops/SkPathOps.h`：路径交集、并集和差集保留填充区域与曲线。
- `third_party/blink/renderer/platform/graphics/filters/fe_offset.cc` 与 `filter_effect.cc`：零位移仍保留 CropRect，滤镜不能仅因 dx/dy 为零就整体删除。
- `ui/views/vector_icons/checkbox_custom.icon`：外轮廓和内孔位于同一路径，使用填充规则表示未选中的边框。
- `components/vector_icons/README.md`：SVG 转换工具仅支持 SVG 子集；`aggregate_vector_icons.py` 只聚合已有 `.icon`，不自动转换 SVG 蒙版。
