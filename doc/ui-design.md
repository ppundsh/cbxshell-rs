# UI Design Guidelines (Fluent)

[繁體中文](ui-design.zh-TW.md)

This document records the visual and interaction rules actually used by the CBX Shell Control Center (`cbx-manager.exe`). Every value is taken from `crates/cbx-manager/src/main.rs` and `render_aa.rs`, so other projects can reproduce the same Windows 11 Fluent look with plain Win32 / GDI (or any immediate-mode renderer).

> Sizes are in **96-DPI design pixels (dp)** and are always converted with `scale_px(px, dpi) = (px × dpi + 48) / 96`. Font sizes are in **pt**, converted to a GDI height of `-(pt × dpi / 72)`.

---

## 1. Principles

1. **Follow the system**: track Windows dark/light mode and the personalization accent color, repainting live on `WM_SETTINGCHANGE` and `WM_DWMCOLORIZATIONCOLORCHANGED`.
2. **Layered materials**: the navigation rail is translucent frosted material, the content area is an opaque panel, cards are solid layers on the panel, and sub-cards sit one step darker.
3. **Pressing never moves things**: a pressed state only changes color. **Text does not shift and borders do not thicken**; feedback is color interpolation plus at most a tiny (+0.8 px) grow.
4. **Short, directional motion**: every animation is 100–220 ms, mostly ease-out, and says where something comes from and where it goes.
5. **Anti-aliasing**: circles, pills and check marks are drawn from a signed distance field (SDF) coverage value instead of jagged GDI primitives.

---

## 2. Layout and Sizing

| Item | Value |
| :--- | :--- |
| Default / minimum window | 1140 × 910 / 880 × 620 |
| Custom title bar height | 48 (Task Manager style; the system caption height if that is taller); DWM's minimize / maximize / close buttons sit at the top |
| Window title | 14 pt Semibold, x = 20, vertically centred between the window edge and the top of the content panel, independent of the rail width |
| Navigation rail width | 195 expanded / 60 collapsed |
| Rail toggle button | 40 × 36 at x = 10, y = 2 (below the title bar), Segoe Fluent Icons `U+E700`; the version / status text sits 8 to its right, vertically centred in the same row |
| First navigation item top | 48, item pitch 44 (pill height 40) |
| Content left edge | rail width + 25 |
| Page title | y = 25, 18 pt Semibold |
| First card top | 75 (+45 when a banner is shown) |
| Card spacing | 14 (16 on some pages) |
| Card padding | 16–18 horizontally, header height 52 (expanders) |
| Scrollbar | 10 wide, 4 from the right edge, minimum thumb 30, 50 per wheel notch |

### Typography

| Use | Typeface | Size / weight |
| :--- | :--- | :--- |
| Page title (hero) | Segoe UI Variable Text | 18 pt / 600 |
| Section title | same | 14 pt / 600 |
| Card title | same | 11 pt / 600 |
| Body, buttons, menus | same | 10 pt / 400 |
| Captions, status line | same | 9 pt / 400 |
| Expander chevrons | Segoe Fluent Icons | `U+E70D` (up) / `U+E70E` (down) |

---

## 3. Color

### 3.1 Neutral tokens (shown as `#RRGGBB`)

| Token | Dark | Light | Use |
| :--- | :--- | :--- | :--- |
| `win_bg` | `#19191C` | `#F3F3F3` | Content panel |
| `sidebar_bg` | `#1E1B1B` | `#ECECEC` | Rail color when no backdrop is available |
| `card_bg` / `card_border` | `#202024` / `#2D2D33` | `#FFFFFF` / `#E5E5E5` | Cards |
| `sub_card_bg` / `sub_card_border` | `#17171B` / `#28282F` | `#F9F9F9` / `#E5E5E5` | Sub-cards |
| `input_bg` / `input_border` | `#1B1B1E` / `#404045` | `#FFFFFF` / `#CCCCCC` | Text boxes |
| `btn_bg` / `btn_border` | `#222020` / `#443D3D` | `#FBFBFB` / `#DCDCDC` | Standard buttons |
| `btn_hover_bg` / `btn_hover_border` | `#2E2B2B` / `#666666` | `#F2F2F2` / `#BDBDBD` | Hover |
| `text_primary` / `secondary` / `muted` / `disabled` | `#F0F0F0` / `#D0D0D0` / `#A0A0A0` / `#888888` | `#1A1A1A` / `#3A3A3A` / `#5C5C5C` / `#9E9E9E` | Text hierarchy |
| `status_ok` / `warn` / `err` | `#5FCB6C` / `#FFB833` / `#EE5555` | `#0F7B0F` / `#9D5D00` / `#C42B1C` | Status |
| `divider` | `#28282F` | `#E5E5E5` | Dividers |
| `caution_bg` / `border` / `text` | `#3D2C18` / `#9E6430` / `#FFE09C` | `#FFF4CE` / `#E6B800` / `#664400` | Caution banner |
| `modal_backdrop` | `#121214` | `#E6E6E6` | Dialog base |
| `scrollbar_track` / `thumb` | `#1C1C20` / `#44444A` | `#EBEBEB` / `#B8B8B8` | Scrollbar |

> The code stores GDI `COLORREF` values (`0x00BBGGRR`); the table has already been converted to ordinary `#RRGGBB`.

### 3.2 Accent derivation

- Read `HKCU\…\Explorer\Accent\AccentPalette` (Light2 / Light1 / Base / Dark1 / Dark2). If unavailable, derive from `DWM\AccentColor`, and fall back to `#0078D4`.
- **Hover**: Base mixed 14% with white. **Pressed**: Dark1. **Checkbox hover**: Base mixed 16% with black.
- **Accent text in dark mode**: Light1, or Light2 if Light1's luminance is < 120. Light mode uses Dark1.
- **Text on accent**: YIQ luminance ≥ 150 → `#111111`, otherwise white.
- **Subtle accent fill**: dark `#202020` + 22% Base; light white + 12% Base.

---

## 4. Materials: Frosted Glass and Panels

| Layer | Implementation |
| :--- | :--- |
| Navigation rail & title bar | Pixel alpha = 0 so **Mica** shows through: the system blurs and tints the desktop wallpaper and keeps it fixed relative to the screen behind the window, so the glow shifts as the window moves. Windows 11 22H2+ uses `DWMWA_SYSTEMBACKDROP_TYPE = 2` (DWMSBT_MAINWINDOW); Windows 11 21H2 uses the original `DWMWA_MICA_EFFECT` (1029); Windows 10 1803+ falls back to `SetWindowCompositionAttribute` acrylic blur (tint `#202020` / `#F3F3F3`, ~80%). While the window is inactive the system replaces Mica with a solid colour. |
| Fallback | When "Transparency effects" is off or the API is unsupported, every translucent pixel is composited onto `sidebar_bg` and made opaque — **an unblurred see-through window is never shown**. |
| Content panel | `win_bg`, anti-aliased top-left corner radius 10, 1 px `card_border` edge. With a backdrop active, **the page background outside the cards** becomes `win_bg` at alpha 0xB4 (~70%) so the same Mica glow shows through, only deeper; cards, text and controls stay opaque. After painting, panel pixels exactly equal to opaque `win_bg` are swapped for the translucent version. |
| Text on the rail | Must use `DrawThemeTextEx(DTT_COMPOSITED)` to keep correct alpha; GDI `TextOut` turns transparent over alpha = 0 pixels. |
| Custom title bar | Keep `WS_CAPTION` (Snap, system menu and taskbar title keep working), extend the client area to the window top in `WM_NCCALCSIZE`, report the top border as `HTTOP` and the rest of the strip as `HTCAPTION` in `WM_NCHITTEST`, and pass every message to `DwmDefWindowProc` first so DWM draws and handles the caption buttons. The title bar shares the rail's frosted backdrop. |
| Coordinates | UI layout y = 0 is the bottom of the title bar: the client rectangle excludes the strip, mouse y is offset by its height, and the rendered UI is blitted below it, so existing layout values stay unchanged. |
| Prerequisites | `DwmExtendFrameIntoClientArea(-1)`, `DWMWA_USE_IMMERSIVE_DARK_MODE` following the theme. |

---

## 5. Corner Radii

> The last two arguments of GDI `RoundRect` are **ellipse diameters**, so the visual radius is half of them; the anti-aliased helpers take the radius directly. The table lists the **visual radius**.

| Element | Visual radius | Notes |
| :--- | :--- | :--- |
| Content panel top-left | 10 | SDF anti-aliased |
| Cards / expander header hover layer | 6 | `RoundRect` 12 |
| Navigation item pill | 8 | anti-aliased, translucent |
| Rail toggle, language button | 8 | anti-aliased, translucent (same as nav items) |
| Flyout frame | 8 | anti-aliased, blurred backdrop |
| Dialog | 4 | `RoundRect` 8 |
| Sub-card | 3 | `RoundRect` 6 |
| Buttons, segmented buttons, menu items | 2.5 | `RoundRect` 5 |
| Text box | 4 | `RoundRect` 8; 2 px accent bottom stroke when focused |
| Scrollbar track / thumb | 3 | `RoundRect` 6 |
| Checkbox | 4 at 18 px, scales with size | SDF |
| Toggle, slider track | full (pill) | SDF |
| Selection indicator | 3 × 16–20 thin pill | accent |

---

## 6. Cards and Sub-cards

- **Card**: `card_bg` fill + 1 px `card_border`. Cards have no shadow; layering is done with tone only.
- **Sub-card**: `sub_card_bg` + `sub_card_border` inside a card for secondary content such as previews and notes.
- **Expander card**:
  - Header height 52; on the right, a toggle (40 × 20), 14 gap, then a chevron (22 wide, 16 from the edge).
  - **Hover**: the whole card grows 1 px on every side (its outline moves outward) and the header fill is 65% of `card_bg` → `btn_hover_bg` while expanded, `btn_hover_bg` while collapsed; when expanded only the header is filled, clipped so the lower corners stay square.
  - **Press**: 140 ms cubic ease-out towards 35% of `card_bg` → `btn_hover_bg`, while the title fades from `text_bright` to `text_muted`.
  - **Expand / collapse**: height interpolated over 160–200 ms cubic ease-out; content is clipped to the current height.
  - **Body inset**: rows in the expanded body are inset a further 16 from both card edges; the header row is unchanged.
  - **Seam**: when expanded (the Live Activity Monitor card included), the header and the body, and the setting groups inside the body (e.g. "Frame Selection Policy" and "Mean Luminance Filter"), are not separated by an inset hairline; instead a 2 px strip of `win_bg` (the page background) cuts across the whole card, borders included, so they read as two stacked sub-cards.
- **Section dividers inside a card**: 1 px `divider`, inset 16, only for description blocks within one function.
- **One function per card**: different functions (e.g. "FFmpeg Component Path" and "Everything Search Helper") are separate cards with the 14 card gap; never split one card into functions with a divider.
- **Cards with sub-settings are always expanders**: sub-settings (e.g. "Allow reading network shares", "Local network only") appear only when expanded, as body rows separated from the header and from each other by seams, with exactly the indent, type and toggle position (52 from the row's right edge) of "Customize Algorithm Parameters". No extra nested sub-card fill. The header may carry status text and secondary buttons (e.g. "Refresh"); clicking those does not toggle the expander.

---

## 7. Navigation Rail

| State | Pill fill (white / black overlay alpha, dark / light) |
| :--- | :--- |
| Rest | none |
| Hover or selected | 16 / 11 |
| Selected and hovered | 10 / 7 |
| Pressed | interpolates to 10 / 7 within 140 ms; text fades from white (or `#1A1A1A`) to `#A6A6A6` (or `#666666`) |

- Pills are inset 10 on both sides. The rail toggle, nav items and the language button share a 40-wide **icon column** with the glyph centred in it, so every icon sits on the same vertical line whether the rail is expanded or collapsed; the label starts 4 after the column and is clipped away when collapsing.
- **Selection indicator**: a 3 × 20 accent pill, 3 from the pill's left edge, written anti-aliased straight into the premultiplied buffer (painting it with GDI and then forcing its bounding box opaque turns the surrounding translucent pixels into a dark outline). On page change it moves over **220 ms cubic ease-out** and "squashes": height × (1 − 0.45 · sin(πp)), shrinking 45% midway before returning to full length.
- Bottom language button: styled like a nav item (transparent; translucent pill on hover / press / open), 50 from the bottom, height 40, 10 side inset; globe in the icon column, then the language name, and an up-chevron that lifts 2 px when open or pressed.
- **Collapsing the rail**: the toggle animates the width between 195 and 60 over 200 ms cubic ease-out while the content area moves left and reflows. Past 0.35 collapse progress the status text is hidden, past 0.5 nav items show icons only; the language button shows only its globe and its flyout keeps a 160 width, extending to the right. The toggle's hover / pressed look matches the nav pills.

---

## 8. Controls

### 8.1 Buttons

| State | Standard | Primary (accent) |
| :--- | :--- | :--- |
| Rest | `btn_bg` / `btn_border` / `text_primary` | Base / Base / contrast text |
| Hover | `btn_hover_bg` / `btn_hover_border` / `text_bright` | Base +14% white |
| Pressed | `sub_card_bg` / `btn_border` / `text_secondary` | Dark1 |
| Disabled | `btn_disabled_bg` / `btn_disabled_border` / `text_disabled`, no interaction | same |

Height 28–30, radius 2.5, centered 10 pt text. In segmented choices (e.g. border style), the selected segment uses primary colors and the others use `sel_unselected_*`.

### 8.2 Checkbox

- Size 16 (lists) / 18 (security-rule rows). Unchecked: 1.15 px border; checked: no border, accent fill.
- Colors: unchecked dark `#202024` fill with `#666666` border (light `#FBFBFB` / `#8A8A8A`); hover `#313136` / `#AAA5A5` (light `#EFEFEF` / `#5C5C5C`); checked Base, hovered Base + 16% black; the check uses the contrast text color.
- **Check animation**: polyline (0.23s, 0.52s) → (0.43s, 0.72s) → (0.78s, 0.29s) (s = box side), drawn left to right over **180 ms cubic ease-out**; unchecking removes it immediately. Stroke width max(0.075s, 1.1).
- **Hit area**: box plus label across the row, with 2 px above and 4 px below of tolerance.

### 8.3 Toggle switch

- Track: 40 × 20 pill. Off (dark): `#262222` fill at alpha 0.5, `#9E9A9A` border, `#CECECE` knob (light: `#F5F5F5` fill, `#606060` border and knob). On: accent fill, knob `#262222` in dark mode or the contrast text color in light mode. All colors and alpha interpolate with the animation value.
- Knob radius grows 5 → 6.5 with 4 margin; on **hover** it grows +0.8 px around its center.
- **Motion**: every 16 ms it closes 35% of the remaining gap (exponential smoothing) and snaps when the gap is < 0.03.

### 8.4 Slider

- Track height 4 (minimum 3); the filled part is an accent pill.
- Thumb is a radius-9 circle; dark mode `#D0D0D0`, `#E8E8E8` and +0.8 radius when hovered or dragged.
- Hover area extends 8 left/right and 10 above/below the track.

### 8.5 Text boxes

Win11 TextBox style; colours are derived from the card colour (mixed with white in dark mode, black in light mode):

| State | Fill | Border | Bottom stroke |
| :--- | :--- | :--- | :--- |
| Rest | card + 6% white (light: +1.5% black) | card + 8% white (light: +7% black) | 1 px, card + 45% white / black |
| Hover | card + 9% white (light: +3.5% black) | same | same |
| Focused | card + 28% black (light: pure white), darker than the card | same | **2 px accent** |
| Disabled | card + 3% | same | same as border |

- Radius 4; the bottom stroke is inset 3 on both sides to follow the corners. Focusing does **not** select or tint the whole text: a click places a 1 px caret under the pointer and dragging selects text; only **the selected characters** get the accent fill and contrast text. `Shift+←/→`, `Home` / `End` adjust the selection, `Ctrl+A` selects all, and `Backspace` / `Delete`, typing and pasting replace the selection.
- **Clear button**: only while editing a non-empty box, inside its right edge (24 wide, inset 3), Segoe Fluent Icons `U+E894`. Subtle fill on hover, lighter fill and dimmer glyph while pressed; acts **on release** — restores the field's default value when it has one, otherwise empties it.
- Numeric fields accept digits only; `Ctrl+A`, `Ctrl+V`, `Enter` (apply) and `Esc` (cancel) are supported.

### 8.6 Scrollbar

Track `scrollbar_track` with `divider` outline; thumb `scrollbar_thumb`, `text_dim` while dragging.

---

## 9. Flyout Menus

- **Fixed content**: a flyout always lists every option (e.g. "All Pipelines | Direct File | IPC Hit | Search Hit | Stream RPC"); the current choice never reorders or hides entries.
- **Placement (ComboBox style)**: when the window has room, the list opens so that **the selected item sits exactly on top of the trigger button**, extending up and down from it; otherwise the whole list is shifted into the window (8 margin), never shortened.
- **Opening motion**: grows from the selected row outwards to the full height over **105 ms quartic ease-out** (`1 − (1 − p)^4`), content clipped to match.
- **Activation**: buttons and items act on **mouse release**; while held they only show the pressed state, and releasing elsewhere does nothing.
- **Background blur (in-app acrylic)**: the pixels under the flyout are blurred with three box-blur passes (≈ Gaussian, radius 12), then a translucent tint (dark `#2C2C2C` / light `#F9F9F9`, alpha 0xD8) and a 1 px `btn_border` frame are applied, radius 8; over the rail the window backdrop also shows through the tint.
- **Item states**: 36 high (language) or 28 (filters), padding 4, radius 4. Hovered and selected rows get a white / black translucent pill (16 dark, 11 light); pressed drops to 9 / 6 with `text_secondary` text. The selected row has a 3 × 16 accent indicator 2 from its left; **pressing the selected row** squashes the indicator by 40% over 120 ms cubic ease-out until release.
- **Dismissal**: choosing an item (on release) or pressing outside closes the flyout; no close button.

### 9.1 Command-bar (subtle) buttons

- Toolbar triggers and actions (the monitor filter drop-downs, "Clear Log") have **no outline**: transparent at rest, `btn_hover_bg` on hover or while their flyout is open, and a darker 55% mix of `card_bg` → `btn_hover_bg` while pressed; the fill extends 2 above and below the hit row, radius 8. Text is `text_primary`, `text_bright` on hover, `text_secondary` pressed.
- Different function groups are separated by a 1 px `divider` vertical line (e.g. between the filters and "Clear Log").
- Ordinary action buttons inside cards (e.g. "Clean thumbnail cache") keep the outlined standard button from §8.1.

---

## 10. Dialogs (Modal)

- A black scrim covers the window: 65% opacity in dark mode, 35% in light mode.
- The card is at most 620 × 518, at least 16 from the window edges, centered; `card_bg`, radius 4, with layered offset shadows (`#121113`, `#0E0C0E`, … expanded 2–4 px).
- 22 horizontal padding, title starts at 18, close button 26 × 26.

---

## 11. Motion Reference

| Animation | Duration | Curve |
| :--- | :--- | :--- |
| Rail collapse / expand | 200 ms | cubic ease-out, width interpolation |
| Page transition (drill-in) | 200 ms | content slides in from 14 below with cubic ease-out; alpha linear 0 → 255 |
| Navigation indicator | 220 ms | cubic ease-out + 45% sine squash |
| Navigation item press | 140 ms | cubic ease-out |
| Expander press | 140 ms | cubic ease-out |
| Expander open/close | 160 ms (sort), 180 ms (security rules, video parameters), 200 ms (monitor) | cubic ease-out |
| Checkbox check | 180 ms | cubic ease-out, drawn left to right |
| Toggle | 35% of remaining gap per frame | exponential smoothing |
| Flyout | 105 ms | quartic ease-out, from the selected row outwards |
| Flyout item press (selected row) | 120 ms | cubic ease-out, indicator squashes 40% |

- **Driver**: a single `WM_TIMER` (16 ms; 10 ms while a checkbox animates or a drag is active) calls `step_animations()`; the timer is killed as soon as nothing is animating, so the idle UI uses no CPU.
- **Partial repaint**: while only the language menu animates, only the rail is invalidated.

---

## 12. Rendering Notes

1. **Double buffering**: the whole window is drawn into a 32-bit top-down DIB and blitted once (`WM_ERASEBKGND` returns 1) to avoid flicker.
2. **Premultiplied alpha**: DIB pixels are premultiplied BGRA; translucent elements are composited with `blend_premul`, and after GDI drawing the `stamp_*` helpers set that area's alpha to 255.
3. **SDF anti-aliasing**: rounded rectangles, circles, pills and check marks convert "distance − radius" into 0–1 coverage and blend.
4. **DPI**: Per-Monitor V2, every size goes through `scale_px`.
5. **Re-entrancy safety**: UI state lives in a `Mutex` accessed with `try_lock`; messages re-entering during modal loops (e.g. the file dialog) only schedule a repaint and can never deadlock.

---

## 13. Porting Checklist

- [ ] Keep the system frame and caption buttons but let the client area cover the caption and paint the title yourself; frame extended over the whole client area, system backdrop set, opaque fallback ready.
- [ ] Dark and light token sets; accent read from the system with hover, pressed and contrast text derived from it.
- [ ] Design at 96 DPI and scale at run time; use Segoe UI Variable Text and Segoe Fluent Icons.
- [ ] Pressed states change color only; hover growth never exceeds 1 px.
- [ ] Every animation ≤ 220 ms, mostly ease-out, and the timer stops when idle.
- [ ] Flyouts grow from their trigger and close on outside click; dialogs dim the background with a scrim.
- [ ] Text on the rail uses an alpha-aware text API.
