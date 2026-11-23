import { useState, useEffect, useMemo, useRef, useDeferredValue, useLayoutEffect } from 'react';
import { EditorState, EditorSelection, StateField, StateEffect, Compartment } from '@codemirror/state';
import { EditorView, keymap, Decoration, WidgetType, ViewPlugin } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap, undo, redo } from '@codemirror/commands';
import { syntaxTree } from '@codemirror/language';
import { markdown } from '@codemirror/lang-markdown';
import { Layout, Upload, Button, Table, Tag, Checkbox, Modal, Input, InputNumber, Space, Tooltip, App as AntApp, List, Tabs, Select, Switch, Card, Statistic, Row, Col, Alert, Drawer, Pagination, Empty, Progress, Spin, Dropdown, Segmented } from 'antd';
import { UploadOutlined, ScanOutlined, DeleteOutlined, TagOutlined, ReloadOutlined, FolderOpenOutlined, CopyOutlined, FolderOutlined, ArrowUpOutlined, HomeOutlined, SearchOutlined, EyeOutlined, PlayCircleOutlined, PauseCircleOutlined, CheckCircleOutlined, ClockCircleOutlined, SyncOutlined, MenuFoldOutlined, MenuUnfoldOutlined, SaveOutlined, EditOutlined, ThunderboltOutlined, FontSizeOutlined, FilePdfOutlined, FileWordOutlined, SettingOutlined, MoreOutlined, BookOutlined, SwapOutlined, QuestionCircleOutlined, StarOutlined } from '@ant-design/icons';
import axios from 'axios';
import { marked } from 'marked';

// OCR 输出常缺少空行分段，开启 breaks 把单个换行渲染为 <br>；gfm 支持表格
marked.setOptions({ gfm: true, breaks: true });

// Markdown 预览可选字体
const MD_FONT_FAMILIES = [
  { value: 'default', label: '默认', css: "-apple-system, 'Segoe UI', 'Microsoft YaHei', sans-serif" },
  { value: 'yahei', label: '微软雅黑', css: "'Microsoft YaHei', 'PingFang SC', sans-serif" },
  { value: 'songti', label: '宋体', css: "SimSun, 'Songti SC', serif" },
  { value: 'heiti', label: '黑体', css: "SimHei, 'Heiti SC', sans-serif" },
  { value: 'kaiti', label: '楷体', css: "KaiTi, 'Kaiti SC', serif" },
  { value: 'mono', label: '等宽', css: "Consolas, 'Courier New', monospace" }
];

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// 统一构建搜索/替换正则：返回 { re, error }，正则非法时把错误信息交给界面行内提示。
// 整词用环视包裹，避免 \b 在中文等非 ASCII 词边界上失效。
// 必须带 m 标志：^ / $ 要匹配每一行的行首、行尾，否则只能匹配整篇的开头和结尾。
function compileSearch(term, { useRegex, caseSensitive, wholeWord }) {
  if (!term) return { re: null, error: '' };
  try {
    let source = useRegex ? term : escapeRegExp(term);
    if (wholeWord) source = `(?<!\\w)(?:${source})(?!\\w)`;
    return { re: new RegExp(source, caseSensitive ? 'gm' : 'gim'), error: '' };
  } catch (error) {
    return { re: null, error: error.message || '正则表达式无效' };
  }
}

// 只替换指定偏移处的那一处匹配。
// 用定长 lookbehind 把位置钉住，再交给原生 replace 展开 $1/$<name>/$&/$$，
// 不做任何 $ 字符串拼接，替换串的语义与 String.prototype.replace 完全一致。
function replaceMatchAt(text, re, index, replacement) {
  const anchored = new RegExp(`(?<=^[\\s\\S]{${index}})(?:${re.source})`, re.flags);
  return text.replace(anchored, replacement);
}

// 收集匹配位置，零长度匹配（如 ^ 或 a*）全部丢弃，避免替换时死循环或插入点爆炸
function collectMatches(text, re) {
  if (!re) return [];
  re.lastIndex = 0;
  return Array.from(text.matchAll(re)).filter((m) => m[0].length > 0);
}

// 历史记录总数上限，避免长时间编辑时撤销栈无限增长
const EDIT_HISTORY_LIMIT = 50;

// ===== Markdown 预览页的本地偏好：快捷键与搜索/替换栏默认可见性 =====
const MD_PREF_DEFAULTS = {
  saveShortcut: 'Ctrl+S',
  findShortcut: 'Ctrl+F',
  replaceShortcut: 'Ctrl+H',
  findBarOpen: false,
  replaceBarOpen: false
};

function readMdPrefs() {
  try {
    const raw = JSON.parse(localStorage.getItem('mdPreviewPrefs') || '{}');
    return { ...MD_PREF_DEFAULTS, ...raw };
  } catch (error) {
    return { ...MD_PREF_DEFAULTS };
  }
}

// 把一次按键事件格式化成「Ctrl+Shift+S」这样的组合键描述
function shortcutFromEvent(event) {
  const key = event.key;
  if (!key || ['Control', 'Meta', 'Alt', 'Shift'].includes(key)) return null;
  const parts = [];
  if (event.ctrlKey || event.metaKey) parts.push('Ctrl');
  if (event.altKey) parts.push('Alt');
  if (event.shiftKey) parts.push('Shift');
  parts.push(key.length === 1 ? key.toUpperCase() : key);
  return parts.join('+');
}

// 判断事件是否命中配置里的组合键；Ctrl 与 Cmd 视为同一修饰键
function matchShortcut(event, combo) {
  if (!combo) return false;
  const parts = combo.split('+').map((s) => s.trim()).filter(Boolean);
  if (!parts.length) return false;
  const key = parts[parts.length - 1].toLowerCase();
  const needCtrl = parts.some((p) => p.toLowerCase() === 'ctrl');
  const needAlt = parts.some((p) => p.toLowerCase() === 'alt');
  const needShift = parts.some((p) => p.toLowerCase() === 'shift');
  if (needCtrl !== (event.ctrlKey || event.metaKey)) return false;
  if (needAlt !== !!event.altKey) return false;
  if (needShift !== !!event.shiftKey) return false;
  return String(event.key || '').toLowerCase() === key;
}

// 快捷键录制输入框：聚焦后按下组合键即可写入
function ShortcutRecorder({ label, value, defaultValue, onChange }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 10 }}>
      <span style={{ width: 88, fontSize: 12, color: 'var(--pm-text-2)' }}>{label}</span>
      <Input
        size="small"
        readOnly
        value={value}
        placeholder="点击后按下组合键"
        style={{ width: 190, cursor: 'pointer' }}
        onFocus={(e) => e.target.select()}
        onKeyDown={(e) => {
          e.preventDefault();
          e.stopPropagation();
          const combo = shortcutFromEvent(e);
          if (combo) onChange(combo);
        }}
      />
      <Button size="small" type="text" onClick={() => onChange(defaultValue)}>
        恢复默认
      </Button>
    </div>
  );
}

// 自定义字体文件的 Select 取值
const CUSTOM_FONT_KEY = '__custom__';

// 注册表字体项形如「宋体 & 新宋体 (TrueType)」，提取可用于 CSS 的字体族名
function fontFamilyFromRegistryName(raw) {
  let name = String(raw || '').trim();
  name = name.replace(/\s*\((TrueType|OpenType|All res|Type 1|PostScript)[^)]*\)\s*$/i, '').trim();
  const first = name.split(/[&,]/)[0].trim();
  return first || name;
}

// 从 Markdown 源码提取标题（跳过代码围栏），编辑/实时预览时用于实时更新目录
function buildHeadingsFromMarkdown(md) {
  const headings = [];
  let inFence = false;
  let offset = 0;
  md.split('\n').forEach((line, index) => {
    const trimmed = line.trim();
    const lineOffset = offset;
    offset += line.length + 1;
    if (trimmed.startsWith('```') || trimmed.startsWith('~~~')) {
      inFence = !inFence;
      return;
    }
    if (inFence) return;
    const match = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (!match) return;
    const text = match[2].replace(/[*_`~]/g, '').trim();
    headings.push({
      id: `md-line-${index}`,
      level: match[1].length,
      text: text || '（无标题）',
      line: index,
      offset: lineOffset,
      length: line.length
    });
  });
  return headings;
}

// ===== 实时预览编辑器（CodeMirror 6）=====
// 旧实现是「透明 textarea + 独立渲染层」两套布局叠加：可见文字的位置来自离屏测量，
// 光标与选区来自 textarea 自身布局，两套布局的误差会按行号累加，滚动越远错位越明显；
// 图片高度只存在于渲染层，textarea 又无法为某一行留出空间，图片必然压住后续文字。
// 现在文本、图片、光标、选区、滚动都由同一个编辑器内核负责，只存在一套布局，
// 不再需要任何坐标同步，也就没有可累积的误差。

const liveThemeCompartment = new Compartment();

// 图片固有尺寸缓存：首次加载后记录宽高，之后滚动重挂载直接按真实比例占位，避免回流跳动
const liveImageSizeCache = new Map();

function applyLiveImageSize(img, size) {
  if (!size || !size.width || !size.height) return;
  img.style.aspectRatio = `${size.width} / ${size.height}`;
  img.style.width = `${size.width}px`;
  img.style.maxWidth = '100%';
  img.style.height = 'auto';
}

// 图片在文档流中作为 widget 存在，高度由编辑器实测，文字自然排在图片上下
class LiveImageWidget extends WidgetType {
  constructor(src, alt) {
    super();
    this.src = src;
    this.alt = alt;
  }

  eq(other) {
    return other.src === this.src && other.alt === this.alt;
  }

  toDOM(view) {
    const wrap = document.createElement('span');
    wrap.className = 'cm-md-image';
    const img = document.createElement('img');
    img.src = this.src;
    img.alt = this.alt || '';
    img.draggable = false;
    const cached = liveImageSizeCache.get(this.src);
    if (cached) {
      applyLiveImageSize(img, cached);
    } else {
      // 加载完成前给一个可点击的占位区，避免图片高度为 0 时点击落空
      wrap.classList.add('cm-md-image-loading');
    }
    img.addEventListener('load', () => {
      wrap.classList.remove('cm-md-image-loading');
      const size = { width: img.naturalWidth, height: img.naturalHeight };
      if (size.width && size.height) {
        liveImageSizeCache.set(this.src, size);
        applyLiveImageSize(img, size);
      }
      view.requestMeasure();
    });
    img.addEventListener('error', () => {
      wrap.classList.remove('cm-md-image-loading');
      wrap.classList.add('cm-md-image-error');
      wrap.textContent = `[图片加载失败] ${this.alt || this.src}`;
      view.requestMeasure();
    });
    // 兜底：触屏、辅助技术或程序化点击可能只派发 click 而没有 mousedown，
    // 此时同样把光标落到图片位置；真实鼠标点击在 mousedown 阶段已由
    // liveImageMouseSelection 处理并把 widget 换回源码，不会重复触发。
    wrap.addEventListener('click', () => {
      if (!wrap.isConnected) return;
      const pos = view.posAtDOM(wrap);
      if (typeof pos !== 'number') return;
      const { main } = view.state.selection;
      if (main.empty && main.head === pos) return;
      view.dispatch({ selection: { anchor: pos } });
      view.focus();
    });
    wrap.appendChild(img);
    return wrap;
  }

  // 返回 false 表示图片内的事件交还编辑器处理：点击任意位置都会把光标定位到
  // 该图片对应的文档位置，从而触发「光标行显示原始 Markdown」；拖拽选区、
  // 双击等也走编辑器原生逻辑，右键菜单保持浏览器默认行为。
  ignoreEvent() {
    return false;
  }
}

// 图片上的按下：不走「屏幕坐标反查文档位置」那套（图片未加载完、占位高度变化时坐标不可靠，
// 且 widget 内部没有文字可落点），而是直接用 DOM 归属算出图片在文档里的起始位置。
// 返回 null 时（非左键、非图片区域）完全交给编辑器默认逻辑，右键菜单保持浏览器行为。
const liveImageMouseSelection = EditorView.mouseSelectionStyle.of((view, event) => {
  if (event.button !== 0) return null;
  const target = event.target;
  const el = target && target.nodeType === 1 ? target : target && target.parentElement;
  const wrap = el && el.closest ? el.closest('.cm-md-image') : null;
  if (!wrap) return null;
  const imagePos = view.posAtDOM(wrap);
  if (typeof imagePos !== 'number') return null;
  let startPos = imagePos;
  return {
    update(update) {
      if (update.docChanged) startPos = update.changes.mapPos(startPos);
    },
    get(ev, extend, multiple) {
      // 按下立刻落在图片起始位置（该行显示源码）；拖拽时从图片位置扩展到当前坐标
      const pos =
        ev === event
          ? startPos
          : view.posAtCoords({ x: ev.clientX, y: ev.clientY }, false) ?? startPos;
      let range = EditorSelection.cursor(pos);
      if (!extend && pos !== startPos) {
        range = EditorSelection.range(Math.min(startPos, pos), Math.max(startPos, pos));
      }
      if (extend) {
        const selection = view.state.selection;
        return selection.replaceRange(selection.main.extend(range.from, range.to));
      }
      if (multiple) return view.state.selection.addRange(range);
      return EditorSelection.create([range]);
    }
  };
});

const LIVE_IMAGE_RE = /^!\[([^\]]*)\]\(\s*<?([^)\s>]+)>?(?:\s+["'][^"']*["'])?\s*\)$/;

function parseLiveImage(text) {
  const matched = LIVE_IMAGE_RE.exec(text.trim());
  return matched ? { alt: matched[1] || '', src: matched[2] } : null;
}

// 预加载文档内所有图片的固有尺寸，让 widget 在首次进入可视区前就能正确占位
function preloadLiveImageSizes(text, onLoaded) {
  const re = /!\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+["'][^"']*["'])?\s*\)/g;
  const seen = new Set();
  let matched;
  while ((matched = re.exec(text)) !== null) {
    const src = matched[1];
    if (!src || seen.has(src) || liveImageSizeCache.has(src)) continue;
    seen.add(src);
    const probe = new Image();
    probe.onload = () => {
      if (probe.naturalWidth && probe.naturalHeight) {
        liveImageSizeCache.set(src, { width: probe.naturalWidth, height: probe.naturalHeight });
        onLoaded();
      }
    };
    probe.onerror = () => {};
    probe.src = src;
  }
}

const liveMatchLineEffect = StateEffect.define();
const liveMatchLineField = StateField.define({
  create: () => -1,
  update(value, tr) {
    for (const effect of tr.effects) {
      if (effect.is(liveMatchLineEffect)) return effect.value;
    }
    return value;
  }
});

const MD_INLINE_CLASSES = {
  StrongEmphasis: 'cm-md-strong',
  Emphasis: 'cm-md-em',
  Strikethrough: 'cm-md-strike',
  InlineCode: 'cm-md-inline-code',
  Link: 'cm-md-link'
};
const MD_HEADING_NAMES = new Set([
  'ATXHeading1',
  'ATXHeading2',
  'ATXHeading3',
  'ATXHeading4',
  'ATXHeading5',
  'ATXHeading6',
  'SetextHeading1',
  'SetextHeading2'
]);

// 构建可视区的 Markdown 装饰：光标行完整保留源码，其余行隐藏标记并渲染样式/图片。
// 装饰只改显示不改文档，光标、选区、撤销都保持原生行为。
function buildLiveDecorations(view) {
  const { state } = view;
  const ranges = [];
  const cursorLineNo = state.doc.lineAt(state.selection.main.head).number;
  const matchLineNo = state.field(liveMatchLineField, false) ?? -1;
  if (matchLineNo > 0 && matchLineNo <= state.doc.lines) {
    ranges.push(Decoration.line({ class: 'cm-md-match' }).range(state.doc.line(matchLineNo).from));
  }
  const touchesCursorLine = (from, to) =>
    state.doc.lineAt(from).number === cursorLineNo || state.doc.lineAt(to).number === cursorLineNo;
  const hide = (from, to) => {
    if (to > from) ranges.push(Decoration.replace({}).range(from, to));
  };

  for (const { from, to } of view.visibleRanges) {
    syntaxTree(state).iterate({
      from,
      to,
      enter: (node) => {
        const name = node.name;
        if (MD_INLINE_CLASSES[name]) {
          if (!touchesCursorLine(node.from, node.to)) {
            ranges.push(Decoration.mark({ class: MD_INLINE_CLASSES[name] }).range(node.from, node.to));
          }
          return;
        }
        if (name === 'Image') {
          if (!touchesCursorLine(node.from, node.to)) {
            const parsed = parseLiveImage(state.doc.sliceString(node.from, node.to));
            if (parsed) {
              ranges.push(
                Decoration.replace({ widget: new LiveImageWidget(parsed.src, parsed.alt) }).range(
                  node.from,
                  node.to
                )
              );
            }
          }
          return false;
        }
        if (MD_HEADING_NAMES.has(name)) {
          const line = state.doc.lineAt(node.from);
          if (line.number !== cursorLineNo) {
            const level = name.slice(-1);
            ranges.push(
              Decoration.line({ class: `cm-md-heading cm-md-heading-${level}` }).range(line.from)
            );
          }
          return;
        }
        if (name === 'HeaderMark') {
          if (!touchesCursorLine(node.from, node.to)) {
            // 连标题标记后的一个空格一起隐藏，与预览效果保持一致
            const next = state.doc.sliceString(node.to, Math.min(node.to + 1, state.doc.length));
            hide(node.from, node.to + (next === ' ' ? 1 : 0));
          }
          return;
        }
        if (name === 'EmphasisMark' || name === 'StrikethroughMark') {
          if (!touchesCursorLine(node.from, node.to)) hide(node.from, node.to);
          return;
        }
        if (name === 'CodeMark') {
          // 只隐藏行内代码的反引号，围栏代码块保持原样
          if (node.node.parent?.name === 'InlineCode' && !touchesCursorLine(node.from, node.to)) {
            hide(node.from, node.to);
          }
          return;
        }
        if (name === 'LinkMark') {
          if (
            (node.node.parent?.name === 'Link' || node.node.parent?.name === 'Image') &&
            !touchesCursorLine(node.from, node.to)
          ) {
            hide(node.from, node.to);
          }
          return;
        }
        if ((name === 'URL' || name === 'LinkTitle') && node.node.parent?.name === 'Link') {
          if (!touchesCursorLine(node.from, node.to)) hide(node.from, node.to);
        }
      }
    });
  }
  return Decoration.set(ranges, true);
}

const liveMarkdownDecorations = ViewPlugin.fromClass(
  class {
    constructor(view) {
      this.decorations = buildLiveDecorations(view);
    }

    update(update) {
      const matchChanged = update.transactions.some((tr) =>
        tr.effects.some((effect) => effect.is(liveMatchLineEffect))
      );
      if (update.docChanged || update.viewportChanged || update.selectionSet || matchChanged) {
        this.decorations = buildLiveDecorations(update.view);
      }
    }
  },
  { decorations: (plugin) => plugin.decorations }
);

// Ctrl+D 删除当前行（含跨行选区），与旧实现语义一致
function deleteLiveLine(view) {
  const { state } = view;
  const selection = state.selection.main;
  const firstLine = state.doc.lineAt(selection.from);
  const lastLine = state.doc.lineAt(selection.to);
  const begin = firstLine.from;
  const end = lastLine.number < state.doc.lines ? state.doc.line(lastLine.number + 1).from : lastLine.to;
  view.dispatch({
    changes: { from: begin, to: end },
    selection: { anchor: begin },
    scrollIntoView: true
  });
  return true;
}

function buildLiveTheme(fontCss, fontSize) {
  const lineHeight = Math.round(fontSize * 1.8 * 100) / 100;
  return EditorView.theme(
    {
      '&': {
        height: '100%',
        fontSize: `${fontSize}px`,
        color: 'var(--pm-text)',
        backgroundColor: 'transparent'
      },
      '.cm-scroller': {
        overflow: 'auto',
        fontFamily: fontCss,
        lineHeight: `${lineHeight}px`,
        scrollbarGutter: 'stable'
      },
      '.cm-content': { padding: '10px 12px', caretColor: 'var(--pm-text)' },
      '.cm-line': { padding: '0' },
      '&.cm-focused': { outline: 'none' },
      '.cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection': {
        backgroundColor: 'var(--pm-selection) !important'
      }
    },
    { dark: false }
  );
}

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function formatBytes(bytes) {
  const size = Number(bytes) || 0;
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(2)} KB`;
  if (size < 1024 * 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(2)} MB`;
  return `${(size / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

const { Header, Content } = Layout;

// 业务状态 → 语义色统一映射：同义状态不再各自取色
const STATUS_TONES = {
  info: { color: 'var(--pm-info)', bg: 'var(--pm-info-bg)', border: 'var(--pm-info-border)' },
  processing: { color: 'var(--pm-processing)', bg: 'var(--pm-processing-bg)', border: 'var(--pm-processing-border)' },
  warning: { color: 'var(--pm-warning)', bg: 'var(--pm-warning-bg)', border: 'var(--pm-warning-border)' },
  success: { color: 'var(--pm-success)', bg: 'var(--pm-success-bg)', border: 'var(--pm-success-border)' },
  error: { color: 'var(--pm-error)', bg: 'var(--pm-error-bg)', border: 'var(--pm-error-border)' },
  neutral: { color: 'var(--pm-text-2)', bg: 'var(--pm-bg-subtle)', border: 'var(--pm-border)' }
};

const DOC_STATUS = {
  uploaded: { label: '已上传', tone: 'info' },
  splitting: { label: '分割中', tone: 'warning' },
  ocr_processing: { label: 'OCR 处理中', tone: 'processing' },
  completed: { label: '已完成', tone: 'success' },
  failed: { label: '失败', tone: 'error' }
};

function statusTagStyle(tone) {
  const t = STATUS_TONES[tone] || STATUS_TONES.neutral;
  return { margin: 0, color: t.color, background: t.bg, borderColor: t.border };
}

// 批量重命名的逐行状态
const RENAME_STATUS = {
  ok: { label: '正常', tone: 'success' },
  unchanged: { label: '无变化', tone: 'neutral' },
  conflict: { label: '冲突', tone: 'error' },
  invalid: { label: '非法', tone: 'warning' },
  missing: { label: '文件不存在', tone: 'error' },
  readonly: { label: '不可写', tone: 'warning' },
  renamed: { label: '成功', tone: 'success' },
  failed: { label: '失败', tone: 'error' }
};

// 视口尺寸断点：内嵌 WebView2 窗口可自由缩放，按宽度切换布局密度
function useViewportSize() {
  const [size, setSize] = useState(() => ({ width: window.innerWidth, height: window.innerHeight }));
  useEffect(() => {
    let frame = 0;
    const onResize = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => setSize({ width: window.innerWidth, height: window.innerHeight }));
    };
    window.addEventListener('resize', onResize);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('resize', onResize);
    };
  }, []);
  return size;
}

function MainApp() {
  // antd v5 推荐：message/modal 通过 App 上下文获取，静态方法在 React 18 下可能不渲染
  const { message, modal } = AntApp.useApp();

  // <1200 紧凑：隐藏次要列、压缩内边距；<1600 常规：收起批量按钮、路径合并进文件名列；>=1920 宽屏
  const { width: viewportWidth, height: viewportHeight } = useViewportSize();
  const isCompact = viewportWidth < 1200;
  const isNarrow = viewportWidth < 1600;
  const isWide = viewportWidth >= 1920;
  const headerIconOnly = viewportWidth < 1500;
  const contentPadding = isCompact ? 12 : isWide ? 28 : 20;
  const showPathColumn = !isNarrow;
  const showCreatedColumn = !isNarrow;
  const showClickColumn = !isNarrow;
  const stackedMdPanes = viewportWidth < 1200;
  const tableScrollX = isCompact ? 1260 : isNarrow ? 1260 : 1810;

  // 预览窗口可拖拽调整高度：0 表示按视口自动计算
  const [modalHeight, setModalHeight] = useState(() => Number(localStorage.getItem('mdPreviewHeight')) || 0);
  const mdPaneHeight = modalHeight
    ? Math.max(240, modalHeight - 150)
    : Math.min(760, Math.max(240, viewportHeight - 340));
  const modalTop = viewportHeight < 800 ? 12 : 40;

  // documents 只保存当前页数据；total/processingCount 由服务端分页接口返回
  const [documents, setDocuments] = useState([]);
  const [total, setTotal] = useState(0);
  const [processingCount, setProcessingCount] = useState(0);
  const [pagination, setPagination] = useState({ current: 1, pageSize: 10 });
  // 大页容量（≥200 条/页）切换为轻量单元格：antd 的 Select/Switch/Tooltip/Tag 每行要实例化十余个
  // 重型组件，1000 行会把主线程阻塞几十秒。这里改成原生控件，交互与数据语义完全一致。
  const liteCells = pagination.pageSize >= 200;

  // 列表区自适应高度：只让表格主体滚动，表头与其他区域常驻
  const tableAreaRef = useRef(null);
  const [tableBodyHeight, setTableBodyHeight] = useState(360);
  useLayoutEffect(() => {
    const el = tableAreaRef.current;
    if (!el) return undefined;
    const compute = () => {
      const header = el.querySelector('.ant-table-thead');
      const pager = el.querySelector('.ant-table-pagination');
      const headerH = header ? header.getBoundingClientRect().height : 41;
      const pagerH = pager ? pager.getBoundingClientRect().height + 16 : 0;
      const next = Math.max(120, Math.round(el.clientHeight - headerH - pagerH - 2));
      setTableBodyHeight((prev) => (Math.abs(prev - next) > 1 ? next : prev));
    };
    compute();
    const ro = new ResizeObserver(compute);
    ro.observe(el);
    return () => ro.disconnect();
  }, [documents.length, total, isCompact, viewportWidth, viewportHeight]);
  const [selectedRows, setSelectedRows] = useState([]);
  const [isScanModalVisible, setIsScanModalVisible] = useState(false);
  const [scanPath, setScanPath] = useState('');
  // 批量添加标签弹窗
  const [isBatchTagModalVisible, setIsBatchTagModalVisible] = useState(false);
  const [batchTagNames, setBatchTagNames] = useState([]);
  const [batchTagLoading, setBatchTagLoading] = useState(false);
  // 批量重命名：表单 → 后端预览 → 确认执行 → 逐条结果
  const [isBatchRenameVisible, setIsBatchRenameVisible] = useState(false);
  const [renameForm, setRenameForm] = useState({
    mode: 'literal',
    find: '',
    replace: '',
    caseSensitive: false,
    mainNameOnly: true,
    cleanWhitespace: false
  });
  const [renamePreview, setRenamePreview] = useState(null);
  const [renamePreviewLoading, setRenamePreviewLoading] = useState(false);
  const [renameExecLoading, setRenameExecLoading] = useState(false);
  const [renameResult, setRenameResult] = useState(null);
  const [renameError, setRenameError] = useState('');
  const [renameTargetCount, setRenameTargetCount] = useState(0);
  // 手动指定的新名：doc_id -> 输入值；提交后随预览一起发给后端校验
  const [renameOverrides, setRenameOverrides] = useState({});
  const [renameEditing, setRenameEditing] = useState({});
  const [renameOnlyConflict, setRenameOnlyConflict] = useState(false);
  const [tags, setTags] = useState([]);
  // 细化的加载态：表格加载、上传、扫描、批量操作、按行OCR
  const [tableLoading, setTableLoading] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [batchLoading, setBatchLoading] = useState(false);
  const [ocrLoadingIds, setOcrLoadingIds] = useState([]);
  // 单篇「从本地 JSON 重建 Markdown」的在途状态
  const [rebuildLoadingIds, setRebuildLoadingIds] = useState([]);
  // 目录浏览器状态：browsePath 为空串时展示盘符列表
  const [browsePath, setBrowsePath] = useState('');
  const [folderParent, setFolderParent] = useState(null);
  const [folderList, setFolderList] = useState([]);
  const [foldersLoading, setFoldersLoading] = useState(false);
  // 搜索关键字：按文件名、标签名即时过滤（服务端模糊匹配）
  const [searchText, setSearchText] = useState('');
  const [pathKeyword, setPathKeyword] = useState('');
  // 过滤器：状态多选、标签多选（服务端过滤，与搜索/分页共用查询参数）
  const [statusFilter, setStatusFilter] = useState([]);
  const [tagFilter, setTagFilter] = useState([]);
  const [ocrDisabledFilter, setOcrDisabledFilter] = useState(null); // null=全部, true=仅禁止, false=仅允许
  const [ocrSwitchLoadingIds, setOcrSwitchLoadingIds] = useState([]);
  const [ratingSavingIds, setRatingSavingIds] = useState([]);
  const [batchRatingVisible, setBatchRatingVisible] = useState(false);
  const [batchRatingValue, setBatchRatingValue] = useState(10);
  const [batchRatingLoading, setBatchRatingLoading] = useState(false);

  // 批量设置 OCR 开关：直接写入目标值（同值重复执行也返回成功）
  const handleBatchOcrDisabled = (disabled) => {
    if (selectedRows.length === 0) {
      message.warning('请先勾选文档');
      return;
    }
    modal.confirm({
      title: disabled ? '批量禁用 OCR' : '批量启用 OCR',
      content: `将把选中的 ${selectedRows.length} 个文档的 OCR 开关设为「${
        disabled ? '禁用' : '启用'
      }」，重复执行结果一致。`,
      okText: disabled ? '禁用' : '启用',
      cancelText: '取消',
      onOk: async () => {
        setBatchLoading(true);
        try {
          const { data } = await axios.post('/api/documents/batch/ocr-disabled', {
            doc_ids: selectedRows,
            disabled
          });
          const text = `成功${disabled ? '禁用' : '启用'} ${data.success} 个，跳过 ${data.skipped} 个${
            data.skipped_missing ? `（文档不存在 ${data.skipped_missing}）` : ''
          }${data.failed ? `，失败 ${data.failed} 个` : ''}`;
          if (data.failed) message.warning(text);
          else message.success(text);
          setSelectedRows([]);
          fetchDocuments(true);
        } catch (error) {
          message.error('批量设置 OCR 失败：' + (error.response?.data?.error || error.message));
        } finally {
          setBatchLoading(false);
        }
      }
    });
  };

  // 批量打分：与单篇评分共用 0~10 语义
  const handleBatchSetRating = async () => {
    if (selectedRows.length === 0) {
      message.warning('请先勾选文档');
      return;
    }
    setBatchRatingLoading(true);
    try {
      const { data } = await axios.post('/api/documents/batch/rating', {
        doc_ids: selectedRows,
        rating: batchRatingValue
      });
      const text = `已为 ${data.success} 个文档设置评分${
        data.skipped ? `，跳过 ${data.skipped} 个（文档不存在）` : ''
      }${data.failed ? `，失败 ${data.failed} 个` : ''}`;
      if (data.failed || data.skipped) message.warning(text);
      else message.success(text);
      setBatchRatingVisible(false);
      setSelectedRows([]);
      fetchDocuments(true);
    } catch (error) {
      message.error('批量评分失败：' + (error.response?.data?.error || error.message));
    } finally {
      setBatchRatingLoading(false);
    }
  };

  // 评分保存：0~10，0 表示未评分
  const handleSetRating = async (record, rating) => {
    setRatingSavingIds((ids) => [...ids, record.id]);
    try {
      await axios.put(`/api/documents/${record.id}/rating`, { rating });
      setDocuments((docs) => docs.map((d) => (d.id === record.id ? { ...d, rating } : d)));
    } catch (error) {
      message.error('评分保存失败：' + (error.response?.data?.error || error.message));
    } finally {
      setRatingSavingIds((ids) => ids.filter((id) => id !== record.id));
    }
  };

  // 系统设置面板：Token（仅回显掩码）、目录、OCR/调度/重试参数
  const [settingsVisible, setSettingsVisible] = useState(false);
  const [settingsLoading, setSettingsLoading] = useState(false);
  const [settingsSaving, setSettingsSaving] = useState(false);
  const [settingsData, setSettingsData] = useState(null);
  const [settingsTokenInput, setSettingsTokenInput] = useState('');
  const [settingsForm, setSettingsForm] = useState({});
  const [settingsTab, setSettingsTab] = useState('basic');
  // 设置面板内的目录选择器：target 为 image/json/md，null 表示收起
  const [settingsPickerTarget, setSettingsPickerTarget] = useState(null);
  const [pickerPath, setPickerPath] = useState('');
  const [pickerParent, setPickerParent] = useState(null);
  const [pickerList, setPickerList] = useState([]);
  const [pickerLoading, setPickerLoading] = useState(false);

  // 全量自动调度器状态
  const [schedulerStatus, setSchedulerStatus] = useState({
    running: false,
    enabled: false,
    completed_count: 0,
    pending_count: 0,
    processing_count: 0,
    active_workers: 0,
    max_workers: 2
  });
  const [schedulerBtnLoading, setSchedulerBtnLoading] = useState(false);

  // Markdown 预览弹窗
  const [previewVisible, setPreviewVisible] = useState(false);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewTitle, setPreviewTitle] = useState('');
  const [previewContent, setPreviewContent] = useState('');
  // 预览弹窗内的原始 JSON 查看
  const [previewDocId, setPreviewDocId] = useState(null);
  // 最近一次打开预览的文档 id：异步响应回来时用于丢弃已切换文档的旧结果
  const previewDocIdRef = useRef(null);
  const [previewJsonPaths, setPreviewJsonPaths] = useState([]);
  const [jsonIndex, setJsonIndex] = useState(0);
  const [jsonContent, setJsonContent] = useState('');
  const [jsonLoading, setJsonLoading] = useState(false);
  // 原始 PDF 对比面板：默认关闭，点击工具栏按钮后在 Markdown 右侧显示
  const [pdfPaneVisible, setPdfPaneVisible] = useState(false);
  const [previewHasPdf, setPreviewHasPdf] = useState(false);
  // 预览窗口内「从本地 JSON 重建 Markdown」：文档状态、在途标记、窗口内结果提示
  const [previewDocStatus, setPreviewDocStatus] = useState('');
  const [previewRebuilding, setPreviewRebuilding] = useState(false);
  const [previewRebuildNotice, setPreviewRebuildNotice] = useState(null);
  // 导出 EPUB / Word：打包在后台线程执行，按钮显示加载态避免重复提交
  const [exportingEpub, setExportingEpub] = useState(false);
  const [exportingDocx, setExportingDocx] = useState(false);
  // 繁简转换进行中（大文档由后端阻塞线程处理）
  const [convertingZh, setConvertingZh] = useState(false);

  // 全文检索抽屉：索引状态、构建、查询与分页
  const [searchDrawerVisible, setSearchDrawerVisible] = useState(false);
  const [searchIndexStatus, setSearchIndexStatus] = useState(null);
  const [searchStatusLoading, setSearchStatusLoading] = useState(false);
  const [searchBuildLoading, setSearchBuildLoading] = useState(false);
  const [searchKeyword, setSearchKeyword] = useState('');
  const [searching, setSearching] = useState(false);
  const [searchResults, setSearchResults] = useState([]);
  const [searchTotal, setSearchTotal] = useState(0);
  const [searchTookMs, setSearchTookMs] = useState(0);
  const [searchPage, setSearchPage] = useState(1);
  const [searchPageSize, setSearchPageSize] = useState(10);
  const [searchExecuted, setSearchExecuted] = useState(false);
  const [searchError, setSearchError] = useState(null);
  const [searchBuildError, setSearchBuildError] = useState(null);
  // 检索模式：smart（分词+加权+AND/OR）| exact（原文连续子串）
  const [searchMode, setSearchMode] = useState('smart');
  const [searchRelaxed, setSearchRelaxed] = useState(false);
  const [searchMatchedTerms, setSearchMatchedTerms] = useState([]);
  const [searchTruncated, setSearchTruncated] = useState(false);
  // 单调递增序号：连续回车时丢弃过期响应，避免旧结果覆盖新结果
  const searchSeqRef = useRef(0);
  // 中文输入法组合期间不触发检索
  const searchComposingRef = useRef(false);

  // 解析 Markdown：给所有标题注入锚点 ID，并同步提取左侧导航条目。
  // 直接基于渲染后的 HTML 提取，保证目录与正文标题一一对应。
  const previewParsed = useMemo(() => {
    const headings = [];
    if (!previewContent) return { headings, html: '' };
    const html = marked.parse(previewContent).replace(
      /<h([1-6])([^>]*)>([\s\S]*?)<\/h\1>/g,
      (_match, level, attrs, inner) => {
        const id = `md-h-${headings.length}`;
        const text = inner
          .replace(/<img[^>]*alt="([^"]*)"[^>]*>/gi, '$1')
          .replace(/<[^>]+>/g, '')
          .replace(/&nbsp;/g, ' ')
          .replace(/&amp;/g, '&')
          .replace(/&lt;/g, '<')
          .replace(/&gt;/g, '>')
          .replace(/&quot;/g, '"')
          .replace(/&#39;/g, "'")
          .trim();
        headings.push({ id, text: text || '（无标题）', level: Number(level) });
        return `<h${level}${attrs} id="${id}">${inner}</h${level}>`;
      }
    );
    return { headings, html };
  }, [previewContent]);

  // 点击左侧目录跳转到对应标题（滚动容器为预览内容区）
  const handleJumpToHeading = (id) => {
    const el = document.getElementById(id);
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
  };

  // 预览窗口交互：编辑/预览/实时预览三种模式、目录折叠、字体、搜索替换、窗口拖动与宽度
  const [editorMode, setEditorMode] = useState('preview'); // preview | edit | live
  const [editContent, setEditContent] = useState('');
  const [tocCollapsed, setTocCollapsed] = useState(false);
  const [mdFontKey, setMdFontKey] = useState(() => localStorage.getItem('mdPreviewFont') || 'default');
  const [mdFontSize, setMdFontSize] = useState(() => Number(localStorage.getItem('mdPreviewSize')) || 14);
  const [searchTerm, setSearchTerm] = useState('');
  const [replaceTerm, setReplaceTerm] = useState('');
  const [searchUseRegex, setSearchUseRegex] = useState(false);
  const [searchCaseSensitive, setSearchCaseSensitive] = useState(false);
  const [searchWholeWord, setSearchWholeWord] = useState(false);
  const [replaceExpanded, setReplaceExpanded] = useState(() => readMdPrefs().replaceBarOpen);
  const [searchCount, setSearchCount] = useState(0);
  const [searchIndex, setSearchIndex] = useState(0);
  const [savingMd, setSavingMd] = useState(false);
  // 预览页偏好（快捷键、搜索/替换栏默认可见）与设置弹窗
  const [mdPrefs, setMdPrefs] = useState(readMdPrefs);
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [searchVisible, setSearchVisible] = useState(() => {
    const p = readMdPrefs();
    return p.findBarOpen || p.replaceBarOpen;
  });
  // Ctrl+S 需要调用后面才定义的 handleSaveMarkdown，用 ref 解耦声明顺序
  const saveMdRef = useRef(null);
  const [modalOffset, setModalOffset] = useState({ x: 0, y: 0 });
  const [modalWidth, setModalWidth] = useState(
    () => Number(localStorage.getItem('mdPreviewWidth')) || Math.min(960, window.innerWidth - 40)
  );
  // 目录宽度 / 正文宽度（px，0 表示铺满）；窗口高度 0 表示按视口自动计算
  const [tocWidth, setTocWidth] = useState(() => Number(localStorage.getItem('mdPreviewTocWidth')) || 200);
  const [bodyWidth, setBodyWidth] = useState(() => Number(localStorage.getItem('mdPreviewBodyWidth')) || 0);
  // 预览页内的评分与标签：打开预览时按文档 id 拉取，修改后同步列表
  const [previewMeta, setPreviewMeta] = useState({ rating: 0, tags: [] });
  const [previewMetaSaving, setPreviewMetaSaving] = useState(false);
  // 自定义字体文件
  const [customFontPath, setCustomFontPath] = useState(() => localStorage.getItem('mdCustomFontPath') || '');
  const [customFontLabel, setCustomFontLabel] = useState(() => localStorage.getItem('mdCustomFontLabel') || '');
  const [fontModalVisible, setFontModalVisible] = useState(false);
  const [fontPathInput, setFontPathInput] = useState('');
  const [fontLoading, setFontLoading] = useState(false);
  // 最近使用的字体文件列表（localStorage 持久化）
  const [recentFonts, setRecentFonts] = useState(() => {
    try {
      return JSON.parse(localStorage.getItem('mdRecentFonts') || '[]');
    } catch {
      return [];
    }
  });
  // 实时预览的搜索命中行
  const [liveMatchLine, setLiveMatchLine] = useState(-1);
  const previewContentRef = useRef(null);
  const editorRef = useRef(null);
  // 实时预览编辑器：宿主节点、编辑器实例、最近一次由编辑器回写的内容
  const liveEditorHostRef = useRef(null);
  const liveViewRef = useRef(null);
  const liveEmittedRef = useRef('');
  const editContentRef = useRef(editContent);
  editContentRef.current = editContent;
  const searchMarksRef = useRef([]);
  const searchInputRef = useRef(null);
  const replaceInputRef = useRef(null);
  // 编辑模式（原生 TextArea）的替换撤销栈：受控组件被程序化赋值后原生撤销栈会失效，
  // 这里只记录「批量/单次替换」的前后快照，用于 Ctrl+Z 一次撤销整批改动
  const editHistoryRef = useRef({ undo: [], redo: [] });
  const loadedCustomFontRef = useRef('');
  const [fontsVersion, setFontsVersion] = useState(0);

  // 编辑模式下目录随内容实时更新（用 deferred 避免输入时阻塞）
  const deferredEditContent = useDeferredValue(editContent);
  const mdHeadings = useMemo(() => buildHeadingsFromMarkdown(deferredEditContent), [deferredEditContent]);
  const tocHeadings = editorMode === 'preview' ? previewParsed.headings : mdHeadings;

  // 统一的搜索正则：非法时 findError 交给界面行内提示，三个模式共用同一份结果
  const { re: searchRegex, error: findError } = useMemo(
    () =>
      compileSearch(searchTerm, {
        useRegex: searchUseRegex,
        caseSensitive: searchCaseSensitive,
        wholeWord: searchWholeWord
      }),
    [searchTerm, searchUseRegex, searchCaseSensitive, searchWholeWord]
  );
  // 预览模式匹配的是 marked 渲染后的文本节点，^ / $ 行锚点与 # 等源码标记都不在其中
  const previewSourceHint =
    editorMode === 'preview' && searchUseRegex && /(?<!\\)[\^$]/.test(searchTerm);

  const mdFontCss = useMemo(() => {
    const fallback = MD_FONT_FAMILIES[0].css;
    if (mdFontKey === CUSTOM_FONT_KEY) return `'md-user-font', ${fallback}`;
    const builtin = MD_FONT_FAMILIES.find((f) => f.value === mdFontKey);
    if (builtin) return builtin.css;
    if (mdFontKey) return `'${mdFontKey}', ${fallback}`;
    return fallback;
  }, [mdFontKey]);

  const fontOptions = useMemo(() => {
    const options = MD_FONT_FAMILIES.map((f) => ({ value: f.value, label: f.label }));
    if (customFontLabel) {
      options.push({ value: CUSTOM_FONT_KEY, label: `自定义：${customFontLabel}` });
    }
    return options;
  }, [customFontLabel]);

  // 创建/销毁实时预览编辑器：文本、图片、光标、选区、滚动共用同一套布局
  useEffect(() => {
    if (editorMode !== 'live' || !previewVisible) return undefined;
    const host = liveEditorHostRef.current;
    if (!host) return undefined;
    const view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: editContentRef.current,
        extensions: [
          history(),
          keymap.of([{ key: 'Mod-d', run: deleteLiveLine }, ...defaultKeymap, ...historyKeymap]),
          EditorView.lineWrapping,
          markdown(),
          liveMatchLineField,
          liveImageMouseSelection,
          liveMarkdownDecorations,
          liveThemeCompartment.of(buildLiveTheme(mdFontCss, mdFontSize)),
          EditorView.updateListener.of((update) => {
            if (!update.docChanged) return;
            const text = update.state.doc.toString();
            liveEmittedRef.current = text;
            setEditContent(text);
          })
        ]
      })
    });
    liveViewRef.current = view;
    view.focus();
    return () => {
      liveViewRef.current = null;
      view.destroy();
    };
    // 只在进入/退出实时预览时重建；字体与内容变化由下方 effect 分别处理
  }, [editorMode, previewVisible]);

  // 外部内容变化（例如全部替换）同步进编辑器；编辑器自身输入不回灌
  useEffect(() => {
    const view = liveViewRef.current;
    if (editorMode !== 'live' || !view) return;
    if (editContent === liveEmittedRef.current) return;
    const current = view.state.doc.toString();
    if (current !== editContent) {
      view.dispatch({ changes: { from: 0, to: current.length, insert: editContent } });
    }
    liveEmittedRef.current = editContent;
  }, [editContent, editorMode]);

  // 字体、字号变化时热更新主题，光标与滚动位置保持不变
  useEffect(() => {
    const view = liveViewRef.current;
    if (!view) return;
    view.dispatch({
      effects: liveThemeCompartment.reconfigure(buildLiveTheme(mdFontCss, mdFontSize))
    });
  }, [mdFontCss, mdFontSize, fontsVersion]);

  // 搜索命中行高亮（state 里记录 1 起始行号）
  useEffect(() => {
    const view = liveViewRef.current;
    if (!view) return;
    view.dispatch({ effects: liveMatchLineEffect.of(liveMatchLine >= 0 ? liveMatchLine + 1 : -1) });
  }, [liveMatchLine, editorMode]);

  // 预加载图片固有尺寸：进入实时预览或内容变化后重新探测，
  // 让 widget 在滚入可视区前就能按真实比例占位，滚动过程中不再跳动
  useEffect(() => {
    if (editorMode !== 'live' || !previewVisible) return undefined;
    const timer = setTimeout(() => {
      preloadLiveImageSizes(editContent, () => liveViewRef.current?.requestMeasure());
    }, 400);
    return () => clearTimeout(timer);
  }, [editorMode, previewVisible, editContent]);

  const mdDirty = editContent !== previewContent;

  // 预览窗口内重建按钮的禁用原因：无 JSON / OCR 处理中 / 有未保存编辑
  let previewRebuildDisabledReason = '';
  if (previewDocStatus === 'splitting' || previewDocStatus === 'ocr_processing') {
    previewRebuildDisabledReason = '文档正在 OCR 处理中，完成后再重建';
  } else if (previewJsonPaths.length === 0) {
    previewRebuildDisabledReason = '该文档没有 OCR JSON，无法重建';
  } else if (mdDirty && editContent) {
    previewRebuildDisabledReason = '有未保存的编辑，请先保存后再重建';
  }

  useEffect(() => {
    localStorage.setItem('mdPreviewFont', mdFontKey);
  }, [mdFontKey]);
  useEffect(() => {
    localStorage.setItem('mdPreviewSize', String(mdFontSize));
  }, [mdFontSize]);
  useEffect(() => {
    localStorage.setItem('mdPreviewWidth', String(modalWidth));
  }, [modalWidth]);
  useEffect(() => {
    localStorage.setItem('mdPreviewTocWidth', String(tocWidth));
  }, [tocWidth]);
  useEffect(() => {
    localStorage.setItem('mdPreviewBodyWidth', String(bodyWidth));
  }, [bodyWidth]);
  useEffect(() => {
    localStorage.setItem('mdPreviewHeight', String(modalHeight));
  }, [modalHeight]);

  // 预览正文关键词高亮：只处理文本节点，避免破坏已渲染的 HTML 结构
  useEffect(() => {
    const root = previewContentRef.current;
    if (!root) return;
    root.querySelectorAll('mark.md-search-hit').forEach((m) => {
      m.replaceWith(document.createTextNode(m.textContent));
    });
    searchMarksRef.current = [];
    setSearchCount(0);
    setSearchIndex(0);
    if (editorMode !== 'preview' || !searchTerm) return;
    const re = searchRegex;
    if (!re) return;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const textNodes = [];
    while (walker.nextNode()) {
      if (walker.currentNode.nodeValue && walker.currentNode.nodeValue.trim()) {
        textNodes.push(walker.currentNode);
      }
    }
    textNodes.forEach((node) => {
      const text = node.nodeValue;
      const matches = Array.from(text.matchAll(re)).filter((m) => m[0].length > 0);
      if (!matches.length) return;
      const frag = document.createDocumentFragment();
      let last = 0;
      matches.forEach((m) => {
        if (m.index > last) frag.appendChild(document.createTextNode(text.slice(last, m.index)));
        const mark = document.createElement('mark');
        mark.className = 'md-search-hit';
        mark.textContent = m[0];
        frag.appendChild(mark);
        last = m.index + m[0].length;
      });
      if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
      node.parentNode.replaceChild(frag, node);
    });
    const marks = Array.from(root.querySelectorAll('mark.md-search-hit'));
    searchMarksRef.current = marks;
    setSearchCount(marks.length);
    if (marks.length) {
      marks[0].classList.add('md-search-active');
      marks[0].scrollIntoView({ block: 'center' });
    }
  }, [previewParsed.html, searchTerm, searchRegex, editorMode, previewVisible]);

  // 编辑/实时预览模式下统计匹配数量（用于“上一个/下一个”与计数展示）
  useEffect(() => {
    if (editorMode === 'preview') return;
    const re = searchRegex;
    if (!re) {
      setSearchCount(0);
      setSearchIndex(0);
      return;
    }
    const total = collectMatches(editContent, re).length;
    setSearchCount(total);
    // 内容变化（例如替换一处）时保持当前位置，越界再回到第一个
    setSearchIndex((prev) => (prev >= total ? 0 : prev));
  }, [editorMode, editContent, searchRegex]);

  // 搜索词或匹配选项变化，回到第一个命中
  useEffect(() => {
    setSearchIndex(0);
  }, [searchRegex]);

  const gotoSearchMatch = (delta) => {
    if (!searchCount) return;
    const next = (searchIndex + delta + searchCount) % searchCount;
    if (editorMode === 'edit') {
      const target = collectMatches(editContent, searchRegex)[next];
      if (target && editorRef.current) {
        editorRef.current.focus();
        editorRef.current.setSelectionRange(target.index, target.index + target[0].length);
      }
    } else if (editorMode === 'live') {
      const target = collectMatches(editContent, searchRegex)[next];
      const view = liveViewRef.current;
      if (target && view) {
        const lineNo = editContent.slice(0, target.index).split('\n').length - 1;
        view.dispatch({
          selection: { anchor: target.index, head: target.index + target[0].length },
          effects: EditorView.scrollIntoView(target.index, { y: 'center' })
        });
        view.focus();
        setLiveMatchLine(lineNo);
      }
    } else {
      const marks = searchMarksRef.current;
      marks.forEach((m, i) => m.classList.toggle('md-search-active', i === next));
      if (marks[next]) marks[next].scrollIntoView({ block: 'center', behavior: 'smooth' });
    }
    setSearchIndex(next);
  };

  // 目录点击：预览滚动到标题；编辑/实时预览把光标移到对应行
  const handleTocClick = (h) => {
    if (editorMode === 'preview') {
      handleJumpToHeading(h.id);
      return;
    }
    if (editorMode === 'edit') {
      const el = editorRef.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(h.offset, h.offset + h.length);
      return;
    }
    const view = liveViewRef.current;
    if (!view) return;
    view.dispatch({
      selection: { anchor: h.offset, head: h.offset + h.length },
      effects: EditorView.scrollIntoView(h.offset, { y: 'center' })
    });
    view.focus();
  };

  // 注册自定义字体到 document.fonts。source 可以是 File 对象（前端直接加载，无需后端）或路径字符串（走 /api/fonts/file）
  const applyCustomFont = async (source, label) => {
    setFontLoading(true);
    try {
      let face;
      if (source instanceof File) {
        const buffer = await source.arrayBuffer();
        face = new FontFace('md-user-font', buffer);
      } else {
        const url = `/api/fonts/file?path=${encodeURIComponent(source)}`;
        face = new FontFace('md-user-font', `url("${url}")`);
      }
      await face.load();
      const stale = [];
      document.fonts.forEach((f) => {
        if (f.family === 'md-user-font') stale.push(f);
      });
      stale.forEach((f) => document.fonts.delete(f));
      document.fonts.add(face);
      // 字体度量变化后需要重新测量实时预览的行高
      setFontsVersion((v) => v + 1);
      const srcPath = source instanceof File ? source.name : source;
      const name = label || (source instanceof File ? source.name : source.split(/[\\/]/).pop());
      setCustomFontPath(srcPath);
      setCustomFontLabel(name);
      setMdFontKey(CUSTOM_FONT_KEY);
      localStorage.setItem('mdCustomFontPath', srcPath);
      localStorage.setItem('mdCustomFontLabel', name);
      // 加入最近使用列表
      setRecentFonts((prev) => {
        const item = { path: srcPath, label: name };
        const next = [item, ...prev.filter((x) => x.path !== srcPath)].slice(0, 8);
        localStorage.setItem('mdRecentFonts', JSON.stringify(next));
        return next;
      });
      message.success(`字体已加载：${name}`);
      return true;
    } catch (error) {
      message.error('字体加载失败：文件不存在或不是有效的字体文件');
      return false;
    } finally {
      setFontLoading(false);
    }
  };

  // 打开预览时恢复上次使用的自定义字体
  useEffect(() => {
    if (!previewVisible) return;
    if (customFontPath && loadedCustomFontRef.current !== customFontPath) {
      applyCustomFont(customFontPath, customFontLabel).then((ok) => {
        if (ok) loadedCustomFontRef.current = customFontPath;
      });
    }
  }, [previewVisible]);

  // 从输入框读取路径并加载字体文件
  const handleLoadFontFile = async () => {
    const path = fontPathInput.trim();
    if (!path) {
      message.warning('请输入字体文件路径');
      return;
    }
    const ok = await applyCustomFont(path, path.split(/[\\/]/).pop());
    if (ok) {
      loadedCustomFontRef.current = path;
      setFontModalVisible(false);
    }
  };

  // 提交一次程序化改动：
  // 实时预览走 CodeMirror 事务（整篇一次事务 → 一次 Ctrl+Z 撤销整批替换）；
  // 编辑模式是受控 TextArea，程序化赋值会清空原生撤销栈，因此额外记录前后快照。
  const commitContentChange = (next) => {
    if (next == null || next === editContentRef.current) return false;
    const view = liveViewRef.current;
    if (editorMode === 'live' && view) {
      const current = view.state.doc.toString();
      if (current !== next) {
        view.dispatch({ changes: { from: 0, to: current.length, insert: next } });
      }
      liveEmittedRef.current = next;
      setEditContent(next);
      return true;
    }
    const history = editHistoryRef.current;
    history.undo.push({ before: editContentRef.current, after: next });
    if (history.undo.length > EDIT_HISTORY_LIMIT) history.undo.shift();
    history.redo = [];
    setEditContent(next);
    return true;
  };

  const undoEditSnapshot = () => {
    const history = editHistoryRef.current;
    const entry = history.undo[history.undo.length - 1];
    if (!entry || editContentRef.current !== entry.after) return false;
    history.undo.pop();
    history.redo.push(entry);
    setEditContent(entry.before);
    return true;
  };

  const redoEditSnapshot = () => {
    const history = editHistoryRef.current;
    const entry = history.redo[history.redo.length - 1];
    if (!entry || editContentRef.current !== entry.before) return false;
    history.redo.pop();
    history.undo.push(entry);
    setEditContent(entry.after);
    return true;
  };

  // 替换串直接交给原生 replace 展开：$1~$99 编号组、$<name> 命名组、$& 整体匹配、$$ 字面 $，
  // 不存在的组号按 JS 规则原样保留，这里不做任何字符串拼接，避免 $ 被二次解释。
  const handleReplace = (onlyCurrent) => {
    if (!searchTerm) {
      message.warning('请输入要查找/替换的内容');
      return;
    }
    if (findError) {
      message.error(`正则表达式无效：${findError}`);
      return;
    }
    const re = searchRegex;
    if (!re) return;
    const source = editContentRef.current;
    const matches = collectMatches(source, re);
    if (!matches.length) {
      message.info('未找到匹配内容');
      return;
    }
    let next;
    let count;
    if (onlyCurrent) {
      const target = matches[Math.min(searchIndex, matches.length - 1)];
      next = replaceMatchAt(source, re, target.index, replaceTerm);
      count = next === source ? 0 : 1;
    } else {
      next = source.replace(re, replaceTerm);
      count = matches.length;
    }
    if (!count || next === source) {
      message.info('未找到可替换的内容');
      return;
    }
    if (editorMode === 'preview') setEditorMode('edit');
    commitContentChange(next);
    message.success(
      onlyCurrent
        ? '已替换 1 处，确认无误后点击“保存”写入文件'
        : `已替换 ${count} 处，可按 Ctrl+Z 整批撤销，确认无误后点击“保存”写入文件`
    );
  };

  // 弹窗内快捷键：Ctrl+F 搜索、Ctrl+H 替换、Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z 撤销重做。
  // 用捕获阶段监听，保证焦点在搜索框、目录或编辑器里都能生效；输入框内的撤销仍交给原生处理。
  useEffect(() => {
    if (!previewVisible) return undefined;
    const onKeyDown = (event) => {
      // 设置弹窗里正在录制快捷键时不触发任何预览快捷键
      if (prefsOpen) return;
      // 保存：默认 Ctrl+S，可在「预览设置」里改
      if (matchShortcut(event, mdPrefs.saveShortcut)) {
        event.preventDefault();
        event.stopPropagation();
        saveMdRef.current?.();
        return;
      }
      // 搜索栏默认隐藏，Ctrl+F 才展开并聚焦
      if (matchShortcut(event, mdPrefs.findShortcut)) {
        event.preventDefault();
        event.stopPropagation();
        setSearchVisible(true);
        setTimeout(() => {
          searchInputRef.current?.focus();
          searchInputRef.current?.select?.();
        }, 0);
        return;
      }
      // 替换栏默认隐藏，Ctrl+H 展开搜索栏 + 替换行并聚焦
      if (matchShortcut(event, mdPrefs.replaceShortcut)) {
        event.preventDefault();
        event.stopPropagation();
        setSearchVisible(true);
        setReplaceExpanded(true);
        const focusReplace = () => {
          const el = replaceInputRef.current;
          if (!el) return false;
          el.focus();
          el.select?.();
          return true;
        };
        if (!focusReplace()) setTimeout(focusReplace, 0);
        return;
      }
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
      const key = String(event.key || '').toLowerCase();
      const target = event.target;
      const tag = (target?.tagName || '').toLowerCase();
      if (key !== 'z' && key !== 'y') return;
      const isRedo = key === 'y' || event.shiftKey;
      // antd TextArea 的真实 DOM 节点在 resizableTextArea.textArea 上
      const mdTextArea = editorRef.current?.resizableTextArea?.textArea;
      const inMdEditor = !!mdTextArea && target === mdTextArea;
      // 搜索/替换等普通输入框：保留浏览器自带的输入撤销
      if (tag === 'input' || (tag === 'textarea' && !inMdEditor)) return;
      if (editorMode === 'live') {
        const view = liveViewRef.current;
        if (!view) return;
        event.preventDefault();
        event.stopPropagation();
        if (isRedo) {
          redo(view);
        } else {
          undo(view);
        }
        view.focus();
        return;
      }
      if (editorMode === 'edit') {
        const done = isRedo ? redoEditSnapshot() : undoEditSnapshot();
        if (done) {
          event.preventDefault();
          event.stopPropagation();
        }
      }
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [previewVisible, editorMode, mdPrefs, prefsOpen]);

  const handleSaveMarkdown = async () => {
    if (!previewDocId || !mdDirty) return;
    setSavingMd(true);
    try {
      await axios.put(`/api/documents/${previewDocId}/markdown`, { content: editContent });
      // 仅更新已保存内容，不切换模式、不重置滚动位置
      setPreviewContent(editContent);
      message.success('Markdown 已保存');
    } catch (error) {
      message.error('保存失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setSavingMd(false);
    }
  };
  // 供 keydown 监听调用（监听声明在 handleSaveMarkdown 之前）
  saveMdRef.current = handleSaveMarkdown;

  // 持久化预览页偏好
  useEffect(() => {
    localStorage.setItem('mdPreviewPrefs', JSON.stringify(mdPrefs));
  }, [mdPrefs]);

  // 拖动标题栏移动预览窗口
  const startModalDrag = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const startX = e.clientX;
    const startY = e.clientY;
    const origin = { ...modalOffset };
    const onMove = (ev) => {
      setModalOffset({
        x: origin.x + ev.clientX - startX,
        y: Math.max(0, origin.y + ev.clientY - startY)
      });
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      document.body.style.userSelect = '';
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  // 拖动右侧边缘调整窗口宽度
  const startModalResize = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = modalWidth;
    const onMove = (ev) => {
      const maxW = Math.max(480, window.innerWidth - 24);
      setModalWidth(Math.max(480, Math.min(maxW, startW + ev.clientX - startX)));
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      document.body.style.userSelect = '';
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  // 拖动底部边缘调整窗口高度（正文可视高度随之变化）
  const startModalHeightResize = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const body = document.querySelector('.ant-modal-body');
    const startY = e.clientY;
    const startH = modalHeight || (body ? body.clientHeight : Math.min(760, viewportHeight - 340) + 150);
    const onMove = (ev) => {
      const maxH = Math.max(360, window.innerHeight - 120);
      setModalHeight(Math.max(360, Math.min(maxH, startH + ev.clientY - startY)));
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      document.body.style.userSelect = '';
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  // 拖动目录右边缘调整目录宽度
  const startTocResize = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = tocWidth;
    const onMove = (ev) => {
      setTocWidth(Math.max(140, Math.min(480, startW + ev.clientX - startX)));
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      document.body.style.userSelect = '';
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  // 拖动正文右边缘调整正文宽度（0 = 铺满剩余空间）
  const startBodyResize = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const wrapper = e.currentTarget.previousElementSibling;
    const startX = e.clientX;
    const startW = bodyWidth || (wrapper ? wrapper.getBoundingClientRect().width : 0);
    const onMove = (ev) => {
      const maxW = Math.max(320, (wrapper ? wrapper.parentElement.clientWidth : window.innerWidth) - 16);
      setBodyWidth(Math.max(280, Math.min(maxW, Math.round(startW + ev.clientX - startX))));
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      document.body.style.userSelect = '';
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };


  // 重命名弹窗
  const [renameVisible, setRenameVisible] = useState(false);
  const [renameLoading, setRenameLoading] = useState(false);
  const [renameDoc, setRenameDoc] = useState(null);
  const [renameValue, setRenameValue] = useState('');

  // 单元格内联编辑：{ docId, field }，field ∈ 'original_path' | 'tags'
  const [editingCell, setEditingCell] = useState(null);
  const [editValue, setEditValue] = useState('');
  const [editSaving, setEditSaving] = useState(false);
  // 进入某个单元格的编辑态
  const startEdit = (record, field, value) => {
    setEditingCell({ docId: record.id, field });
    setEditValue(value);
  };
  const cancelEdit = () => {
    setEditingCell(null);
    setEditValue('');
  };
  // 原始路径内联保存（移动+可能重命名）
  const saveInlineMove = async (record) => {
    const path = editValue.trim();
    if (!path) {
      message.warning('路径不能为空');
      return;
    }
    setEditSaving(true);
    try {
      await axios.put(`/api/documents/${record.id}/move`, { path });
      message.success('文件已移动/重命名');
      setEditingCell(null);
      setEditValue('');
      fetchDocuments(true);
    } catch (error) {
      message.error('移动失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setEditSaving(false);
    }
  };
  // 标签内联保存：对比新旧标签集合，分别调用 add / remove
  const saveInlineTags = async (record, newNames) => {
    const oldTags = record.tags || [];
    const oldNames = new Set(oldTags.map(t => t.name));
    const newSet = new Set(newNames);
    // 无变化时直接退出编辑态，不发起请求
    if (oldNames.size === newSet.size && [...newSet].every(n => oldNames.has(n))) {
      cancelEdit();
      return;
    }
    const toAdd = [...newSet].filter(n => !oldNames.has(n));
    const toRemove = oldTags.filter(t => !newSet.has(t.name));
    setEditSaving(true);
    try {
      await Promise.all([
        ...toAdd.map(n => axios.post(`/api/documents/${record.id}/tags`, { tag_name: n })),
        ...toRemove.map(t => axios.delete(`/api/documents/${record.id}/tags/${t.id}`))
      ]);
      message.success('标签已更新');
      setEditingCell(null);
      fetchTags();
      fetchDocuments(true);
    } catch (error) {
      message.error('标签更新失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setEditSaving(false);
    }
  };

  // 服务端分页查询参数：用 ref 保存最新值，保证轮询定时器读到的不是旧闭包
  const queryRef = useRef({ page: 1, pageSize: 10, keyword: '', pathKeyword: '', statuses: [], tagIds: [], sort_by: 'updated_at', sort_order: 'desc' });

  // 排序状态：{ field, order }，order ∈ 'ascend' | 'descend' | null；用于 antd Table 受控 sortOrder
  const [sortState, setSortState] = useState({ field: 'updated_at', order: 'descend' });

  // 可拖拽列宽：仅文件名与原始路径两列，持久化到 localStorage
  const COL_WIDTH_KEY = 'pm.table.colWidths.v1';
  const DEFAULT_COL_WIDTHS = { filename: 300, original_path: 300 };
  const COL_WIDTH_LIMITS = { filename: [160, 640], original_path: [180, 640] };
  const clampCol = (key, value) => {
    const [min, max] = COL_WIDTH_LIMITS[key];
    const n = Number(value);
    return Number.isFinite(n) ? Math.min(max, Math.max(min, Math.round(n))) : DEFAULT_COL_WIDTHS[key];
  };
  const [colWidths, setColWidths] = useState(() => {
    try {
      const raw = localStorage.getItem(COL_WIDTH_KEY);
      const parsed = raw ? JSON.parse(raw) : {};
      return {
        filename: clampCol('filename', parsed.filename),
        original_path: clampCol('original_path', parsed.original_path)
      };
    } catch {
      return { ...DEFAULT_COL_WIDTHS };
    }
  });
  const colWidthsRef = useRef(colWidths);
  colWidthsRef.current = colWidths;

  // mousemove/mouseup 挂在 document 上，拖出表头也能继续调整
  const startColResize = (key, event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const startX = event.clientX;
    const startWidth = colWidthsRef.current[key];
    const [min, max] = COL_WIDTH_LIMITS[key];
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    const onMove = (e) => {
      const next = Math.min(max, Math.max(min, Math.round(startWidth + e.clientX - startX)));
      setColWidths((prev) => ({ ...prev, [key]: next }));
    };
    const onUp = () => {
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      try {
        localStorage.setItem(COL_WIDTH_KEY, JSON.stringify(colWidthsRef.current));
      } catch {
        // 存储不可用时仅本次会话生效
      }
    };
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  };

  const resetColWidth = (key) => {
    const next = { ...colWidthsRef.current, [key]: DEFAULT_COL_WIDTHS[key] };
    setColWidths(next);
    try {
      localStorage.setItem(COL_WIDTH_KEY, JSON.stringify(next));
    } catch {
      // 忽略存储错误
    }
    message.success(`已恢复默认列宽（${DEFAULT_COL_WIDTHS[key]}px）`);
  };

  const resizableTitle = (label, key) => (
    <div className="pm-col-title">
      <span>{label}</span>
      <span
        className="pm-col-resizer"
        title="拖动调整列宽，双击恢复默认"
        onMouseDown={(e) => startColResize(key, e)}
        onDoubleClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          resetColWidth(key);
        }}
        onClick={(e) => e.stopPropagation()}
      />
    </div>
  );

  // 静默刷新参数：轮询时不触发表格遮罩、不弹错误提示；overrides 可临时指定 page/pageSize/keyword
  // 请求乱序守卫：每次发起列表请求递增序号，仅最新请求允许落库/弹错/结束loading。
  // 防止快速搜索/切换过滤/翻页时先发后至的旧响应覆盖新数据。
  const fetchSeqRef = useRef(0);
  const fetchDocuments = async (silent = false, overrides = {}) => {
    const mySeq = ++fetchSeqRef.current;
    const params = { ...queryRef.current, ...overrides };
    queryRef.current = params;
    if (!silent) setTableLoading(true);
    try {
      const response = await axios.get('/api/documents', {
        params: {
          page: params.page,
          page_size: params.pageSize,
          ...(params.keyword ? { keyword: params.keyword } : {}),
          ...(params.pathKeyword ? { path_keyword: params.pathKeyword } : {}),
          ...(params.statuses?.length ? { status: params.statuses.join(',') } : {}),
          ...(params.tagIds?.length ? { tag_id: params.tagIds.join(',') } : {}),
          ...(params.ocrDisabled !== undefined && params.ocrDisabled !== null ? { ocr_disabled: params.ocrDisabled } : {}),
          ...(params.sort_by ? { sort_by: params.sort_by, sort_order: params.sort_order } : {})
        }
      });
      // 期间已有更新的请求发出（或旧轮询响应晚到），丢弃本次响应
      if (mySeq !== fetchSeqRef.current) return;
      const data = response.data;
      // 末页数据被删空等场景：请求页超出范围时自动回退到最后一页
      if (data.items.length === 0 && data.total > 0 && params.page > 1) {
        const lastPage = Math.ceil(data.total / data.page_size);
        return fetchDocuments(silent, { page: lastPage });
      }
      setDocuments(data.items);
      setTotal(data.total);
      setProcessingCount(data.processing_count || 0);
      setPagination({ current: data.page, pageSize: data.page_size });
      queryRef.current = {
        page: data.page,
        pageSize: data.page_size,
        keyword: params.keyword,
        pathKeyword: params.pathKeyword || '',
        statuses: params.statuses || [],
        tagIds: params.tagIds || [],
        ocrDisabled: params.ocrDisabled !== undefined ? params.ocrDisabled : null,
        sort_by: params.sort_by || 'created_at',
        sort_order: params.sort_order || 'desc'
      };
    } catch (error) {
      // 旧请求失败不弹错（新请求仍在途）
      if (mySeq !== fetchSeqRef.current) return;
      if (!silent) message.error('获取文档列表失败');
    } finally {
      if (!silent && mySeq === fetchSeqRef.current) setTableLoading(false);
    }
  };

  const fetchTags = async () => {
    try {
      const response = await axios.get('/api/tags');
      setTags(response.data);
    } catch (error) {
      message.error('获取标签列表失败');
    }
  };

  const fetchSchedulerStatus = async () => {
    try {
      const response = await axios.get('/api/scheduler/status');
      setSchedulerStatus(response.data);
    } catch (error) {
      // 静默失败
    }
  };

  const [statsRefreshing, setStatsRefreshing] = useState(false);
  const handleRefreshStats = async () => {
    setStatsRefreshing(true);
    try {
      await fetchSchedulerStatus();
    } finally {
      setStatsRefreshing(false);
    }
  };

  const handleToggleScheduler = async () => {
    setSchedulerBtnLoading(true);
    try {
      if (schedulerStatus.running || schedulerStatus.enabled) {
        const res = await axios.post('/api/scheduler/stop');
        setSchedulerStatus(res.data);
        message.info('已暂停自动调度（正在执行的任务会继续跑完）');
      } else {
        const res = await axios.post('/api/scheduler/start');
        setSchedulerStatus(res.data);
        message.success('已开启后台自动全量OCR调度！');
      }
    } catch (error) {
      message.error('调度器操作失败: ' + (error.response?.data?.error || error.message));
    } finally {
      setSchedulerBtnLoading(false);
    }
  };

  useEffect(() => {
    fetchDocuments();
    fetchTags();
    fetchSchedulerStatus();
  }, []);

  // 是否存在后台处理中的文档：服务端全局计数，不受当前页/搜索影响
  const hasProcessing = processingCount > 0;

  // 轮询：每5秒静默刷新当前页与调度器状态看板。
  // 大页容量下整页重绘代价太高，只刷新调度器状态，列表由用户手动刷新。
  useEffect(() => {
    const timer = setInterval(() => {
      fetchSchedulerStatus();
      if (pagination.pageSize >= 200) return;
      if (hasProcessing || schedulerStatus.running) {
        fetchDocuments(true);
      }
    }, 5000);
    return () => clearInterval(timer);
  }, [hasProcessing, schedulerStatus.running, pagination.pageSize]);

  // 搜索改为服务端查询：按回车触发，避免高频输入产生过多请求
  const handleSearch = () => {
    fetchDocuments(false, { page: 1, keyword: searchText.trim() });
  };
  // 输入框清空（allowClear 点 X）时立即重置搜索
  const handleSearchChange = (e) => {
    const v = e.target.value;
    setSearchText(v);
    if (!v && searchText) {
      fetchDocuments(false, { page: 1, keyword: '' });
    }
  };

  // 原始路径搜索：与文件名关键词相互独立，两者同时填写时后端取交集
  const handlePathSearch = () => {
    fetchDocuments(false, { page: 1, pathKeyword: pathKeyword.trim() });
  };
  const handlePathChange = (e) => {
    const v = e.target.value;
    setPathKeyword(v);
    if (!v && pathKeyword) {
      fetchDocuments(false, { page: 1, pathKeyword: '' });
    }
  };

  // 状态/标签过滤器：变更后回到第 1 页重新查询（其他条件保持不变，由 queryRef 合并）
  const handleStatusFilterChange = (vals) => {
    setStatusFilter(vals);
    fetchDocuments(false, { page: 1, statuses: vals });
  };
  const handleTagFilterChange = (vals) => {
    setTagFilter(vals);
    fetchDocuments(false, { page: 1, tagIds: vals });
  };

  const handleOcrDisabledFilterChange = (val) => {
    // antd Select allowClear 清空时 val 为 undefined，归一化为 null（不过滤）
    const v = val === undefined ? null : val;
    setOcrDisabledFilter(v);
    fetchDocuments(false, { page: 1, ocrDisabled: v });
  };

  // 切换文档的OCR禁止开关
  const handleToggleOcrDisabled = async (doc, disabled) => {
    setOcrSwitchLoadingIds(ids => [...ids, doc.id]);
    try {
      await axios.put(`/api/documents/${doc.id}/ocr-disabled`, { ocr_disabled: disabled });
      message.success(disabled ? '已禁止OCR' : '已开启OCR');
      fetchDocuments(true);
    } catch (error) {
      message.error('更新失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setOcrSwitchLoadingIds(ids => ids.filter(id => id !== doc.id));
    }
  };

  // 状态转换通知：记录上一轮处理中的文档，本轮到达终态时提示结果
  const prevProcessingRef = useRef(new Set());
  useEffect(() => {
    const processingNow = new Set(
      documents
        .filter(doc => doc.status === 'splitting' || doc.status === 'ocr_processing')
        .map(doc => doc.id)
    );
    const prev = prevProcessingRef.current;
    documents.forEach(doc => {
      if (prev.has(doc.id) && !processingNow.has(doc.id)) {
        if (doc.status === 'completed') {
          message.success(`《${doc.filename}》OCR处理完成`);
        } else if (doc.status === 'failed') {
          message.error(`《${doc.filename}》OCR失败：${doc.error_message || '未知错误'}`);
        }
      }
    });
    prevProcessingRef.current = processingNow;
  }, [documents]);

  const handleUpload = async ({ file }) => {
    setUploading(true);
    const formData = new FormData();
    formData.append('file', file);

    try {
      await axios.post('/api/documents', formData, {
        headers: { 'Content-Type': 'multipart/form-data' }
      });
      message.success('文件上传成功');
      fetchDocuments(false, { page: 1 });
    } catch (error) {
      message.error('文件上传失败');
    } finally {
      setUploading(false);
    }

    return false;
  };

  const handleOCR = async (docId) => {
    // ocrLoadingIds 仅覆盖提交请求的短暂在途时间；持续转圈由服务端 status 驱动
    setOcrLoadingIds(ids => [...ids, docId]);
    try {
      await axios.post(`/api/documents/${docId}/ocr`);
      message.success('OCR任务已提交，正在后台处理');
      // 立即登记：即便任务快速失败、轮询未观察到处理中状态，终态通知也能触发
      prevProcessingRef.current.add(docId);
      fetchDocuments(true);
    } catch (error) {
      if (error.response?.status === 409) {
        message.warning('该文档正在处理中，请勿重复提交');
      } else {
        message.error('OCR任务提交失败：' + (error.response?.data?.error || '未知错误'));
      }
    } finally {
      setOcrLoadingIds(ids => ids.filter(id => id !== docId));
    }
  };

  const handleBatchOCR = async () => {
    if (selectedRows.length === 0) {
      message.warning('请选择文档');
      return;
    }

    setBatchLoading(true);
    try {
      const { data } = await axios.post('/api/documents/batch/ocr', { doc_ids: selectedRows });
      const { accepted, skipped, details } = data;
      // 登记已受理任务，保证快速失败时终态通知也能触发
      (details || []).forEach(item => {
        if (item.result === 'accepted') prevProcessingRef.current.add(item.doc_id);
      });
      if (accepted > 0 && skipped === 0) {
        message.success(`已提交 ${accepted} 个OCR任务，可在列表中查看处理进度`);
      } else if (accepted > 0) {
        message.warning(`已提交 ${accepted} 个任务，跳过 ${skipped} 个（处理中或不存在）`);
      } else {
        message.warning(`没有可提交的任务，跳过 ${skipped} 个（均在处理中或不存在）`);
      }
      setSelectedRows([]);
      fetchDocuments(true);
    } catch (error) {
      message.error('批量OCR提交失败');
    } finally {
      setBatchLoading(false);
    }
  };

  // 单篇：用本地 OCR JSON 重建 Markdown（不调用 MinerU）
  const handleRebuildMarkdown = (record) => {
    modal.confirm({
      title: '从本地 JSON 重建 Markdown',
      content: `将用「${record.filename}」已保存的 OCR JSON 重新生成 Markdown（包含表格），不会调用 MinerU，也不影响 OCR 状态。旧 Markdown 会自动备份到同目录。`,
      okText: '开始重建',
      onOk: async () => {
        setRebuildLoadingIds(ids => [...ids, record.id]);
        try {
          const { data } = await axios.post(`/api/documents/${record.id}/rebuild-markdown`);
          message.success(
            `重建成功：${(data.bytes / 1024).toFixed(1)} KB` +
            (data.backup_path ? '，旧文件已备份' : '')
          );
          fetchDocuments(true);
        } catch (error) {
          const reason = error.response?.data?.reason
            || error.response?.data?.error
            || error.message;
          message.error('重建失败：' + reason);
        } finally {
          setRebuildLoadingIds(ids => ids.filter(id => id !== record.id));
        }
      }
    });
  };

  // 批量：逐篇重建，汇总成功/跳过/失败明细
  const handleBatchRebuildMarkdown = async () => {
    if (selectedRows.length === 0) {
      message.warning('请选择文档');
      return;
    }
    setBatchLoading(true);
    try {
      const { data } = await axios.post('/api/documents/batch/rebuild-markdown', {
        doc_ids: selectedRows
      });
      const { success = 0, skipped = 0, failed = 0, details = [] } = data;
      const problems = details.filter(item => item.result !== 'success');
      if (problems.length === 0) {
        message.success(`批量重建完成：成功 ${success} 个`);
      } else {
        modal.info({
          title: `批量重建完成：成功 ${success} 个，跳过 ${skipped} 个，失败 ${failed} 个`,
          width: 640,
          content: (
            <div style={{ maxHeight: 360, overflow: 'auto' }}>
              {problems.map(item => (
                <div key={item.doc_id} style={{ marginBottom: 6 }}>
                  <Tag color={item.result === 'skipped' ? 'orange' : 'red'}>
                    {item.result === 'skipped' ? '跳过' : '失败'}
                  </Tag>
                  <span style={{ fontWeight: 500 }}>#{item.doc_id}</span>
                  <span style={{ marginLeft: 8 }}>{item.filename || '（未知文档）'}</span>
                  <div style={{ color: 'var(--pm-text-3)', marginTop: 2 }}>{item.reason || '未知原因'}</div>
                </div>
              ))}
            </div>
          )
        });
      }
      setSelectedRows([]);
      fetchDocuments(true);
    } catch (error) {
      message.error('批量重建失败：' + (error.response?.data?.error || error.message));
    } finally {
      setBatchLoading(false);
    }
  };

  const handleDelete = async (docId) => {
    modal.confirm({
      title: '确认删除',
      content: '删除后无法恢复，确定要删除吗？',
      onOk: async () => {
        try {
          await axios.delete(`/api/documents/${docId}`);
          message.success('删除成功');
          fetchDocuments();
        } catch (error) {
          message.error('删除失败');
        }
      }
    });
  };

  const handleBatchDelete = async () => {
    if (selectedRows.length === 0) {
      message.warning('请选择文档');
      return;
    }
    
    modal.confirm({
      title: '确认批量删除',
      content: `确定要删除选中的 ${selectedRows.length} 个文档吗？删除后无法恢复。`,
      onOk: async () => {
        setBatchLoading(true);
        try {
          // 并发删除，逐个统计成功/失败，避免串行中途失败导致状态不一致
          const results = await Promise.allSettled(
            selectedRows.map(docId => axios.delete(`/api/documents/${docId}`))
          );
          const fulfilled = results.filter(r => r.status === 'fulfilled').length;
          const rejected = results.length - fulfilled;
          if (rejected === 0) {
            message.success(`批量删除成功，共删除 ${fulfilled} 个文档`);
          } else {
            message.warning(`删除完成：成功 ${fulfilled} 个，失败 ${rejected} 个`);
          }
          setSelectedRows([]);
          fetchDocuments();
        } finally {
          setBatchLoading(false);
        }
      }
    });
  };

  // 打开批量重命名弹窗（仅对勾选范围生效）
  const openBatchRenameModal = () => {
    if (selectedRows.length === 0) {
      message.warning('请先勾选需要重命名的文档');
      return;
    }
    setRenameTargetCount(selectedRows.length);
    setRenameForm({ mode: 'literal', find: '', replace: '', caseSensitive: false, mainNameOnly: true, cleanWhitespace: false });
    setRenamePreview(null);
    setRenameResult(null);
    setRenameError('');
    setRenameOverrides({});
    setRenameEditing({});
    setRenameOnlyConflict(false);
    setIsBatchRenameVisible(true);
  };

  const renamePayload = (overrides = renameOverrides) => ({
    doc_ids: selectedRows,
    mode: renameForm.mode,
    find: renameForm.find,
    replace: renameForm.replace,
    case_sensitive: renameForm.caseSensitive,
    main_name_only: renameForm.mainNameOnly,
    clean_whitespace: renameForm.cleanWhitespace,
    overrides: Object.entries(overrides).map(([docId, newFilename]) => ({
      doc_id: Number(docId),
      new_filename: newFilename
    }))
  });

  // 无论是点“预览”还是提交手动改名，都走同一套后端校验
  const runRenamePreview = async (overrides) => {
    if (!renameForm.find && Object.keys(overrides).length === 0) {
      message.warning('请填写查找内容');
      return;
    }
    setRenamePreviewLoading(true);
    setRenameError('');
    setRenameResult(null);
    try {
      const { data } = await axios.post('/api/documents/batch/rename/preview', renamePayload(overrides));
      setRenamePreview(data);
      if (!data.summary?.ok) message.info('没有可改名的文档，请调整规则后再预览');
    } catch (e) {
      setRenamePreview(null);
      setRenameError(e?.response?.data?.error || e.message || '预览失败');
    } finally {
      setRenamePreviewLoading(false);
    }
  };

  const handleRenamePreview = () => runRenamePreview(renameOverrides);

  // 提交/恢复某一行的手动新名；value=null 表示恢复为规则计算结果
  const commitRenameOverride = (docId, value) => {
    const next = { ...renameOverrides };
    if (value === null || value === undefined) {
      delete next[docId];
    } else {
      next[docId] = value;
    }
    setRenameOverrides(next);
    setRenameEditing((m) => {
      const copy = { ...m };
      delete copy[docId];
      return copy;
    });
    runRenamePreview(next);
  };

  const handleRenameExecute = () => {
    const summary = renamePreview?.summary;
    if (!summary?.ok) return;
    const skipped = summary.total - summary.ok;
    modal.confirm({
      title: '确认批量重命名',
      content: `将重命名 ${summary.ok} 个文档，跳过 ${skipped} 条无法执行的行（其中冲突 ${summary.conflict || 0} 条）。文件与数据库会同步更新，是否继续？`,
      okText: '开始重命名',
      cancelText: '取消',
      onOk: async () => {
        setRenameExecLoading(true);
        try {
          const { data } = await axios.post('/api/documents/batch/rename', renamePayload());
          setRenameResult(data);
          if (data.summary?.success) message.success(`已重命名 ${data.summary.success} 个文档`);
          if (data.summary?.failed) message.warning(`${data.summary.failed} 个文档重命名失败`);
          setSelectedRows([]);
          fetchDocuments();
        } catch (e) {
          message.error(e?.response?.data?.error || '批量重命名失败');
        } finally {
          setRenameExecLoading(false);
        }
      }
    });
  };

  const closeBatchRename = () => {
    setIsBatchRenameVisible(false);
    setRenamePreview(null);
    setRenameResult(null);
    setRenameError('');
    setRenameOverrides({});
    setRenameEditing({});
    setRenameOnlyConflict(false);
  };

  // 打开批量添加标签弹窗
  const openBatchTagModal = () => {
    setBatchTagNames([]);
    setIsBatchTagModalVisible(true);
  };

  // 批量为选中文档添加标签（后端对 文档×标签 组合幂等去重）
  const handleBatchAddTags = async () => {
    const names = batchTagNames.map(n => n.trim()).filter(Boolean);
    if (names.length === 0) {
      message.warning('请选择或输入至少一个标签');
      return;
    }
    setBatchTagLoading(true);
    try {
      const results = await Promise.allSettled(
        selectedRows.flatMap(docId => names.map(name => axios.post(`/api/documents/${docId}/tags`, { tag_name: name })))
      );
      const fulfilled = results.filter(r => r.status === 'fulfilled').length;
      const rejected = results.length - fulfilled;
      if (rejected === 0) {
        message.success(`已为 ${selectedRows.length} 个文档添加 ${names.length} 个标签`);
      } else {
        message.warning(`部分标签添加失败：成功 ${fulfilled} 项，失败 ${rejected} 项`);
      }
      setIsBatchTagModalVisible(false);
      setBatchTagNames([]);
      setSelectedRows([]);
      fetchTags();
      fetchDocuments(true);
    } finally {
      setBatchTagLoading(false);
    }
  };

  const handleRemoveTag = async (docId, tagId) => {
    try {
      await axios.delete(`/api/documents/${docId}/tags/${tagId}`);
      message.success('标签移除成功');
      fetchDocuments();
    } catch (error) {
      message.error('标签移除失败');
    }
  };

  // 打开扫描弹窗并加载盘符/根目录列表
  const openScanModal = () => {
    setIsScanModalVisible(true);
    setScanPath('');
    fetchFolders('');
  };

  // 浏览目录：path 为空串时后端返回盘符列表
  const fetchFolders = async (path = '') => {
    setFoldersLoading(true);
    try {
      const { data } = await axios.get('/api/folders', { params: path ? { path } : {} });
      setBrowsePath(data.path);
      setFolderParent(data.parent);
      setFolderList(data.folders);
    } catch (error) {
      message.error('读取目录失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setFoldersLoading(false);
    }
  };

  // 手动输入路径后跳转浏览
  const handleJumpToPath = () => {
    if (!scanPath.trim()) {
      message.warning('请输入目录路径');
      return;
    }
    fetchFolders(scanPath.trim());
  };

  const handleScanDirectory = async (dirPath) => {
    const target = (dirPath ?? browsePath ?? '').trim();
    if (!target) {
      message.warning('请先选择或输入要扫描的目录');
      return;
    }

    setScanning(true);
    try {
      const response = await axios.post('/api/documents/scan', { directory: target });
      const { added, skipped, duplicates, message: msg } = response.data;
      // 后端未找到PDF时只返回 message，不含 added/skipped
      if (added === undefined) {
        message.info(msg || '扫描完成，未找到PDF文件');
      } else {
        let text = `${msg}，新增 ${added} 个文件，跳过 ${skipped} 个文件`;
        if (duplicates > 0) {
          text += `，识别内容重复（已改名/移动）${duplicates} 个文件`;
        }
        message.success(text);
      }
      fetchDocuments(false, { page: 1 });
      setIsScanModalVisible(false);
      setScanPath('');
      setBrowsePath('');
      setFolderList([]);
    } catch (error) {
      message.error('扫描失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setScanning(false);
    }
  };

  // -------------------------------------------------------------
  // 系统设置面板
  // -------------------------------------------------------------
  const SETTINGS_FOLDER_FIELDS = [
    { target: 'image', key: 'image_folder', label: '图片目录', statKey: 'image_dir' },
    { target: 'json', key: 'json_folder', label: 'JSON 目录', statKey: 'json_dir' },
    { target: 'md', key: 'md_folder', label: 'Markdown 目录', statKey: 'md_dir' },
    { target: 'index', key: 'search_index_folder', label: '索引目录', statKey: 'search_index_dir' },
    { target: 'epub', key: 'epub_folder', label: '导出目录（EPUB / Word）', statKey: 'epub_dir' }
  ];

  // OCR 数值/枚举配置：范围与后端校验一致；warn 只做软性风险提示，不阻止保存
  const SETTINGS_FIELD_GROUPS = [
    {
      key: 'schedule',
      label: '调度与额度',
      fields: [
        { key: 'ocr_daily_upload_limit', label: '每日上传上限', unit: '个分块/天', min: 1, max: 20000, step: 100, desc: '达到该值（预留 1% 余量）后暂停投递，次日自动恢复。', warn: (v) => v > 5000 ? '超过官方常见额度 5000/天，可能被 MinerU 拒绝' : '' },
        { key: 'ocr_upload_interval', label: '批次上传间隔', unit: '秒', min: 30, max: 600, step: 5, desc: '每个上传批次之间的强制休眠，用于遵守官方频控。', warn: (v) => v < 60 ? '小于 60 秒可能触发官方频控限流' : '' },
        { key: 'batch_doc_limit', label: '单批聚合文档数', unit: '本', min: 1, max: 200, step: 1, desc: '每次从待处理队列最多拉取的文档数。', warn: (v) => v > 60 ? '过大时单批失败的影响范围更大' : '' },
        { key: 'batch_chunk_limit', label: '单批分块上限', unit: '个', min: 1, max: 50, step: 1, desc: '官方单批最多 50 个文件，默认 48 预留容错空间。', warn: (v) => v > 48 ? '已贴近官方 50 个上限，建议保留余量' : '' },
        { key: 'max_pages', label: '单文件页数上限', unit: '页', min: 1, max: 5000, step: 10, desc: '超过该页数的 PDF 会被自动切分为多个分块后提交。', warn: (v) => v > 600 ? '超过 MinerU 单文件常见页数上限，可能被拒绝' : '' },
        { key: 'max_file_size_mb', label: '单文件大小上限', unit: 'MB', min: 1, max: 2048, step: 10, desc: '上传与切块判断共用；超过该大小会自动分块。', warn: (v) => v > 500 ? '大文件上传耗时与失败率显著上升' : '' },
        { key: 'scheduler_sort_mode', label: '调度排序', type: 'select', options: [{ value: 'shortest_first', label: '页数优先（默认）' }, { value: 'id_asc', label: '按 ID 顺序' }], desc: '决定待处理文档的先后顺序。' }
      ]
    },
    {
      key: 'retry',
      label: '超时与重试',
      fields: [
        { key: 'mineru_poll_timeout', label: '批次轮询超时', unit: '秒', min: 60, max: 86400, step: 60, desc: '超过该时长仍未落定的批次按失败处理，云端记录仍可续跑。', warn: (v) => v > 43200 ? '超过 12 小时的等待意义有限' : '' },
        { key: 'mineru_poll_interval', label: '批次轮询间隔', unit: '秒', min: 2, max: 120, step: 1, desc: '收割线程两次查询批次状态的间隔。', warn: (v) => v < 5 ? '小于 5 秒容易触发查询限流' : '' },
        { key: 'ocr_fetch_max_retries', label: '状态查询重试次数', unit: '次', min: 0, max: 10, step: 1, desc: '单次查询失败后的重试次数（总请求数 = 次数 + 1）。' },
        { key: 'ocr_fetch_timeout', label: '状态查询超时', unit: '秒', min: 5, max: 300, step: 5, desc: '单次批次状态查询的超时时间。' },
        { key: 'ocr_download_max_retries', label: 'ZIP 下载重试次数', unit: '次', min: 0, max: 10, step: 1, desc: '结果 ZIP 下载/解压失败后的单轮重试次数。' },
        { key: 'ocr_download_timeout', label: 'ZIP 下载超时', unit: '秒', min: 30, max: 3600, step: 30, desc: '单个结果 ZIP 的下载超时时间。' },
        { key: 'ocr_download_max_cycles', label: '取结果最大轮次', unit: '轮', min: 1, max: 20, step: 1, desc: '跨收割轮次连续失败达到该值即判定重试耗尽。' },
        { key: 'ocr_retry_backoff_base', label: '首次退避', unit: '秒', min: 0.5, max: 60, step: 0.5, desc: '指数退避的首次等待时间。' },
        { key: 'ocr_retry_backoff_max', label: '退避封顶', unit: '秒', min: 1, max: 600, step: 1, desc: '退避时间的上限，必须不小于首次退避。' },
        { key: 'ocr_retry_jitter_ratio', label: '退避抖动比例', unit: '', min: 0, max: 1, step: 0.05, desc: '在退避时间上叠加 ±该比例的随机抖动，避免并发请求同步。' }
      ]
    },
    {
      key: 'resume',
      label: '续传',
      fields: [
        { key: 'resume_upload_url_ttl', label: '预签名 URL 有效期', unit: '秒', min: 60, max: 86400, step: 60, desc: '超过有效期不再续传，剩余分块改用新批次投递。' },
        { key: 'resume_max_upload_attempts', label: '同批次最大续传次数', unit: '次', min: 1, max: 20, step: 1, desc: '超过后放弃剩余分块并重新排队。' }
      ]
    }
  ];

  const applySettingsData = (data) => {
    setSettingsData(data);
    setSettingsForm({
      ...(data.values || {}),
      image_folder: data.image_folder || '',
      json_folder: data.json_folder || '',
      md_folder: data.md_folder || '',
      search_index_folder: data.search_index_folder || '',
      epub_folder: data.epub_folder || '',
      zh_t2s_mapping: data.zh_t2s_mapping || ''
    });
  };

  const updateSettingsField = (key, value) => {
    setSettingsForm((prev) => ({ ...prev, [key]: value }));
  };

  const handleRestoreDefaults = () => {
    const defaults = settingsData?.defaults;
    if (!defaults) return;
    setSettingsForm((prev) => ({ ...prev, ...defaults }));
    message.info('已填入默认值（来自 .env / 内置默认），点击保存后生效');
  };

  const renderSettingsField = (field) => {
    const value = settingsForm[field.key];
    const warnText =
      field.warn && value !== null && value !== undefined && value !== ''
        ? field.warn(Number(value)) || ''
        : '';
    return (
      <Row gutter={16} key={field.key} style={{ marginBottom: 14 }}>
        <Col span={10}>
          <div style={{ fontWeight: 500 }}>{field.label}</div>
          <div style={{ color: 'var(--pm-text-3)', fontSize: 12 }}>{field.desc}</div>
        </Col>
        <Col span={14}>
          {field.type === 'select' ? (
            <Select
              style={{ width: '100%' }}
              value={value}
              onChange={(v) => updateSettingsField(field.key, v)}
              options={field.options}
            />
          ) : (
            <InputNumber
              style={{ width: '100%' }}
              min={field.min}
              max={field.max}
              step={field.step}
              value={value}
              onChange={(v) => updateSettingsField(field.key, v)}
              addonAfter={field.unit || undefined}
            />
          )}
          <div style={{ color: 'var(--pm-text-3)', fontSize: 12, marginTop: 2 }}>
            默认：{settingsData?.defaults?.[field.key]}　范围：{field.min} ~ {field.max}
            {field.unit ? ` ${field.unit}` : ''}
          </div>
          {warnText && (
            <div style={{ color: 'var(--pm-warning-text)', fontSize: 12, marginTop: 2 }}>{warnText}</div>
          )}
        </Col>
      </Row>
    );
  };

  const openSettings = async () => {
    setSettingsVisible(true);
    setSettingsLoading(true);
    setSettingsTokenInput('');
    setSettingsPickerTarget(null);
    setSettingsTab('basic');
    try {
      const { data } = await axios.get('/api/settings');
      applySettingsData(data);
    } catch (error) {
      message.error('读取设置失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setSettingsLoading(false);
    }
  };

  // 设置面板内的目录浏览（与扫描弹窗的浏览状态隔离，互不干扰）
  const loadPickerFolders = async (path = '') => {
    setPickerLoading(true);
    try {
      const { data } = await axios.get('/api/folders', { params: path ? { path } : {} });
      setPickerPath(data.path);
      setPickerParent(data.parent);
      setPickerList(data.folders || []);
    } catch (error) {
      message.error('读取目录失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setPickerLoading(false);
    }
  };

  const openSettingsPicker = (target) => {
    setSettingsPickerTarget(target);
    loadPickerFolders('');
  };

  const handlePickerChoose = () => {
    if (!pickerPath) {
      message.warning('请先进入并选择目标目录');
      return;
    }
    const field = SETTINGS_FOLDER_FIELDS.find(item => item.target === settingsPickerTarget);
    if (field) {
      updateSettingsField(field.key, pickerPath);
    }
    setSettingsPickerTarget(null);
  };

  const handleSaveSettings = async () => {
    const token = settingsTokenInput.trim();
    const apiUrl = (settingsForm.mineru_api_url || '').trim();
    if (!apiUrl || !/^https?:\/\//.test(apiUrl) || /\s/.test(apiUrl)) {
      message.warning('请填写合法的 MinerU API 基址（http:// 或 https:// 开头）');
      return;
    }
    for (const field of SETTINGS_FOLDER_FIELDS) {
      const value = (settingsForm[field.key] || '').trim();
      if (!value) {
        message.warning(`请填写${field.label}`);
        return;
      }
      if (/[*?"<>|]/.test(value)) {
        message.warning(`${field.label}包含非法字符（* ? " < > |）`);
        return;
      }
      if (!/^[A-Za-z]:[\\/]|^\\\\/.test(value)) {
        message.warning(`${field.label}请填写绝对路径`);
        return;
      }
    }
    const numericFields = SETTINGS_FIELD_GROUPS.flatMap((group) => group.fields);
    for (const field of numericFields) {
      if (field.type === 'select') {
        if (!field.options.some((opt) => opt.value === settingsForm[field.key])) {
          message.warning(`请选择${field.label}`);
          return;
        }
        continue;
      }
      const raw = settingsForm[field.key];
      if (raw === null || raw === undefined || raw === '') {
        message.warning(`请填写${field.label}`);
        return;
      }
      const value = Number(raw);
      if (!Number.isFinite(value) || value < field.min || value > field.max) {
        message.error(`${field.label}需在 ${field.min} ~ ${field.max}${field.unit ? ` ${field.unit}` : ''} 之间`);
        return;
      }
    }
    if (Number(settingsForm.ocr_retry_backoff_max) < Number(settingsForm.ocr_retry_backoff_base)) {
      message.error('退避封顶不能小于首次退避');
      return;
    }
    setSettingsSaving(true);
    try {
      const payload = {
        mineru_api_url: apiUrl,
        image_folder: (settingsForm.image_folder || '').trim(),
        json_folder: (settingsForm.json_folder || '').trim(),
        md_folder: (settingsForm.md_folder || '').trim(),
        search_index_folder: (settingsForm.search_index_folder || '').trim(),
        epub_folder: (settingsForm.epub_folder || '').trim(),
        zh_t2s_mapping: settingsForm.zh_t2s_mapping ?? ''
      };
      numericFields.forEach((field) => {
        payload[field.key] =
          field.type === 'select' ? settingsForm[field.key] : Number(settingsForm[field.key]);
      });
      if (token) payload.mineru_token = token;
      const { data } = await axios.put('/api/settings', payload);
      applySettingsData(data);
      setSettingsTokenInput('');
      setSettingsVisible(false);
      if (data.needs_rebuild) {
        // 索引目录已切换但新目录没有可用索引：引导立即重建，避免检索继续命中旧目录
        message.warning(data.message || '索引目录已变更，需要重建索引');
        setSearchDrawerVisible(true);
        refreshSearchStatus();
        modal.confirm({
          title: '索引目录已变更，需要重建索引',
          content: '新索引目录中还没有可用索引，重建完成前无法检索。是否立即全量重建？',
          okText: '立即重建',
          cancelText: '稍后',
          onOk: () => doSearchBuild('full')
        });
      } else {
        message.success(data.message || '设置已保存并生效');
        refreshSearchStatus(true);
      }
    } catch (error) {
      message.error('保存设置失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setSettingsSaving(false);
    }
  };

  // 复制文本到剪贴板（含非安全上下文兜底）
  const copyTextToClipboard = async (text, successMsg) => {
    try {
      await navigator.clipboard.writeText(text);
      message.success(successMsg);
    } catch (error) {
      const textarea = document.createElement('textarea');
      textarea.value = text;
      textarea.style.position = 'fixed';
      textarea.style.opacity = '0';
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand('copy');
      document.body.removeChild(textarea);
      message.success(successMsg);
    }
  };

  // 复制 OCR 生成的 Markdown 文件路径到剪贴板
  const handleCopyMarkdownPath = (path) => copyTextToClipboard(path, 'Markdown 文件路径已复制到剪贴板');

  // 复制预览中的 Markdown 原始内容到剪贴板
  const handleCopyMarkdownContent = () => copyTextToClipboard(previewContent, 'Markdown 内容已复制到剪贴板');

  // 导出 EPUB：有未保存编辑时导出屏幕上的内容，否则由后端读取磁盘 Markdown
  const handleExportEpub = async () => {
    if (!previewDocId || exportingEpub) return;
    setExportingEpub(true);
    try {
      const payload = mdDirty && editContent ? { content: editContent } : {};
      const { data } = await axios.post(`/api/documents/${previewDocId}/export-epub`, payload);
      if (data.missing_images > 0) {
        message.warning(`EPUB 已导出，但有 ${data.missing_images} 张图片缺失，仅保留图片说明文字`);
      }
      message.success(
        `EPUB 已导出（${data.chapters} 章 / ${data.images} 张图片）：${data.path}`,
        6
      );
    } catch (error) {
      message.error('导出 EPUB 失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setExportingEpub(false);
    }
  };

  // 导出 Word：与 EPUB 语义一致——有未保存编辑时导出屏幕上的内容，否则由后端读取磁盘 Markdown
  const handleExportDocx = async () => {
    if (!previewDocId || exportingDocx) return;
    setExportingDocx(true);
    try {
      const payload = mdDirty && editContent ? { content: editContent } : {};
      const { data } = await axios.post(`/api/documents/${previewDocId}/export-docx`, payload);
      if (data.missing_images > 0) {
        message.warning(
          `Word 已导出，但有 ${data.missing_images} 张图片缺失，已在正文占位并在文末列出`
        );
      }
      message.success(
        `Word 已导出（${data.paragraphs} 段 / ${data.tables} 张表 / ${data.images} 张图，${data.elapsed_ms} ms）：${data.path}`,
        6
      );
    } catch (error) {
      message.error('导出 Word 失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setExportingDocx(false);
    }
  };

  // 繁简转换：转换结果进入编辑区（标记未保存），由用户检查后手动保存
  const handleConvertZh = async (direction) => {
    if (!previewDocId || convertingZh) return;
    const source = mdDirty && editContent ? editContent : previewContent;
    if (!source) return;
    setConvertingZh(true);
    try {
      const { data } = await axios.post('/api/convert-zh', { content: source, direction });
      setEditContent(data.content);
      if (editorMode === 'preview') setEditorMode('live');
      message.success(
        direction === 't2s' ? '繁→简转换完成，请检查后保存' : '简→繁转换完成，请检查后保存'
      );
    } catch (error) {
      message.error('繁简转换失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setConvertingZh(false);
    }
  };

  // 在线预览 OCR 生成的 Markdown
  const handlePreviewMarkdown = async (doc) => {
    // 同一预览窗口内的重复触发（刷新/重建/重复点击）不再计数
    const sameWindowOpen = previewVisible && previewDocId === doc.id;
    setPreviewVisible(true);
    setPreviewLoading(true);
    setPreviewTitle(doc.filename);
    setPreviewContent('');
    setPreviewDocId(doc.id);
    previewDocIdRef.current = doc.id;
    // 每次打开按偏好决定搜索/替换栏是否默认展开
    const prefs = readMdPrefs();
    setSearchVisible(prefs.findBarOpen || prefs.replaceBarOpen);
    setReplaceExpanded(prefs.replaceBarOpen);
    // 先用列表里已有的评分/标签占位，随后以接口返回的最新值为准
    setPreviewMeta({ rating: doc.rating || 0, tags: doc.tags || [] });
    axios
      .get(`/api/documents/${doc.id}`)
      .then(({ data }) => {
        if (previewDocIdRef.current !== doc.id) return;
        setPreviewMeta({ rating: data.rating || 0, tags: data.tags || [] });
      })
      .catch(() => {});
    // 打开预览计数：每次打开只调用一次；窗口内切换编辑/预览、重建、刷新都不会重复计数
    if (!sameWindowOpen) {
      axios
        .post(`/api/documents/${doc.id}/click`)
        .then(({ data }) => {
          setDocuments((docs) => docs.map((d) => (d.id === doc.id ? { ...d, click_count: data.click_count } : d)));
        })
        .catch(() => {});
    }
    setPreviewDocStatus(doc.status || '');
    setPreviewJsonPaths([]);
    setJsonIndex(0);
    setJsonContent('');
    setPreviewRebuildNotice(null);
    // 重置预览窗口内的编辑/搜索/布局状态
    setEditContent('');
    setEditorMode('preview');
    setTocCollapsed(false);
    setSearchTerm('');
    setReplaceTerm('');
    setSearchUseRegex(false);
    setSearchCaseSensitive(false);
    setSearchWholeWord(false);
    editHistoryRef.current = { undo: [], redo: [] };
    setSearchCount(0);
    setSearchIndex(0);
    setModalOffset({ x: 0, y: 0 });
    setLiveMatchLine(-1);
    setPdfPaneVisible(false);
    setPreviewHasPdf(false);
    try {
      const { data } = await axios.get(`/api/documents/${doc.id}/markdown`);
      setPreviewTitle(data.filename || doc.filename);
      const raw = data.content || '';
      setPreviewContent(raw || '（Markdown 内容为空）');
      setEditContent(raw);
      setPreviewJsonPaths(data.json_paths || []);
      setPreviewHasPdf(!!data.has_pdf);
    } catch (error) {
      message.error('加载Markdown失败：' + (error.response?.data?.error || '未知错误'));
      setPreviewVisible(false);
    } finally {
      setPreviewLoading(false);
    }
  };

  // 预览页内直接打分：与列表的评分语义一致（0 表示清除）
  const handlePreviewRating = async (rating) => {
    if (!previewDocId) return;
    setPreviewMetaSaving(true);
    try {
      await axios.put(`/api/documents/${previewDocId}/rating`, { rating });
      setPreviewMeta((m) => ({ ...m, rating }));
      setDocuments((docs) => docs.map((d) => (d.id === previewDocId ? { ...d, rating } : d)));
      message.success(rating ? `已评分 ${rating} 分` : '已清除评分');
    } catch (error) {
      message.error('评分保存失败：' + (error.response?.data?.error || error.message));
    } finally {
      setPreviewMetaSaving(false);
    }
  };

  // 预览页内直接改标签：对比新旧集合后分别调用 add / remove
  const handlePreviewTagsChange = async (names) => {
    if (!previewDocId) return;
    const oldTags = previewMeta.tags || [];
    const oldNames = new Set(oldTags.map((t) => t.name));
    const newSet = new Set(names);
    const toAdd = [...newSet].filter((n) => !oldNames.has(n));
    const toRemove = oldTags.filter((t) => !newSet.has(t.name));
    if (!toAdd.length && !toRemove.length) return;
    setPreviewMetaSaving(true);
    try {
      await Promise.all([
        ...toAdd.map((n) => axios.post(`/api/documents/${previewDocId}/tags`, { tag_name: n })),
        ...toRemove.map((t) => axios.delete(`/api/documents/${previewDocId}/tags/${t.id}`))
      ]);
      const { data } = await axios.get(`/api/documents/${previewDocId}`);
      setPreviewMeta({ rating: data.rating || 0, tags: data.tags || [] });
      setDocuments((docs) => docs.map((d) => (d.id === previewDocId ? { ...d, tags: data.tags || [] } : d)));
      fetchTags();
      message.success('标签已更新');
    } catch (error) {
      message.error('标签更新失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setPreviewMetaSaving(false);
    }
  };

  // 预览窗口内重建：完成后直接重新拉取 Markdown 刷新预览，无需关闭窗口
  const handlePreviewRebuild = async () => {
    if (!previewDocId || previewRebuilding) return;
    setPreviewRebuilding(true);
    setPreviewRebuildNotice(null);
    try {
      const { data } = await axios.post(`/api/documents/${previewDocId}/rebuild-markdown`);
      // 重新读取产物，保证预览展示的就是刚写回文件的内容
      const { data: latest } = await axios.get(`/api/documents/${previewDocId}/markdown`);
      const raw = latest.content || '';
      setPreviewContent(raw || '（Markdown 内容为空）');
      setEditContent(raw);
      setPreviewTitle(latest.filename || previewTitle);
      setPreviewJsonPaths(latest.json_paths || []);
      setPreviewHasPdf(!!latest.has_pdf);
      setPreviewRebuildNotice({
        type: 'success',
        text: `重建完成，预览已刷新（${(data.bytes / 1024).toFixed(1)} KB）` +
          (data.backup_path ? '，旧 Markdown 已备份到同目录' : '')
      });
    } catch (error) {
      const payload = error.response?.data || {};
      const skipped = payload.result === 'skipped';
      const reason = payload.reason || payload.error || error.message;
      setPreviewRebuildNotice({
        type: skipped ? 'warning' : 'error',
        text: `${skipped ? '重建被跳过' : '重建失败'}：${reason}`
      });
    } finally {
      setPreviewRebuilding(false);
    }
  };

  // ---------------- 全文检索 ----------------

  const refreshSearchStatus = async (silent = false) => {
    if (!silent) setSearchStatusLoading(true);
    try {
      const { data } = await axios.get('/api/search/index/status');
      setSearchIndexStatus(data);
    } catch (error) {
      setSearchIndexStatus((prev) => ({
        ...(prev || {}),
        state: 'error',
        ready: false,
        running: false,
        doc_count: prev?.doc_count || 0,
        source_doc_count: prev?.source_doc_count || 0,
        index_size: prev?.index_size || 0,
        last_error: error.response?.data?.error || error.message
      }));
    } finally {
      if (!silent) setSearchStatusLoading(false);
    }
  };

  const openSearchDrawer = () => {
    setSearchDrawerVisible(true);
    refreshSearchStatus();
  };

  // 构建进行中时轮询进度；抽屉关闭或构建结束自动停止
  useEffect(() => {
    if (!searchDrawerVisible || !searchIndexStatus?.running) return undefined;
    const timer = setInterval(() => refreshSearchStatus(true), 1200);
    return () => clearInterval(timer);
  }, [searchDrawerVisible, searchIndexStatus?.running]);

  const doSearchBuild = async (mode) => {
    setSearchBuildLoading(true);
    setSearchBuildError(null);
    try {
      await axios.post(`/api/search/index/${mode === 'full' ? 'rebuild' : 'update'}`);
      message.success(mode === 'full' ? '已开始全量重建索引' : '已开始增量更新索引');
      await refreshSearchStatus();
    } catch (error) {
      const status = error.response?.status;
      setSearchBuildError({
        type: status === 409 ? 'warning' : 'error',
        text: status === 409
          ? '索引构建正在进行中，请等待当前任务结束'
          : '启动索引构建失败：' + (error.response?.data?.error || error.message)
      });
    } finally {
      setSearchBuildLoading(false);
    }
  };

  const handleSearchBuild = (mode) => {
    if (mode !== 'full') {
      doSearchBuild(mode);
      return;
    }
    modal.confirm({
      title: '全量重建索引',
      content: '将重新读取所有 Markdown 并替换索引目录。构建期间旧索引仍可检索，OCR 不受影响。是否继续？',
      okText: '开始重建',
      cancelText: '取消',
      onOk: () => doSearchBuild('full')
    });
  };

  const runSearch = async (targetPage = 1, sizeOverride, modeOverride) => {
    const keyword = searchKeyword.trim();
    if (!keyword) {
      handleSearchClear();
      return;
    }
    const mode = modeOverride || searchMode;
    const pageSize = sizeOverride ?? searchPageSize;
    const seq = searchSeqRef.current + 1;
    searchSeqRef.current = seq;
    setSearching(true);
    setSearchExecuted(true);
    setSearchError(null);
    try {
      const { data } = await axios.get('/api/search', {
        params: { q: keyword, page: targetPage, page_size: pageSize, mode }
      });
      if (seq !== searchSeqRef.current) return; // 已有更新的请求，丢弃过期响应
      setSearchResults(data.results || []);
      setSearchTotal(data.total || 0);
      setSearchTookMs(data.took_ms || 0);
      setSearchPage(data.page || targetPage);
      setSearchRelaxed(!!data.relaxed);
      setSearchMatchedTerms(data.matched_terms || []);
      setSearchTruncated(!!data.truncated);
    } catch (error) {
      if (seq !== searchSeqRef.current) return;
      const status = error.response?.status;
      setSearchResults([]);
      setSearchTotal(0);
      setSearchError({
        type: status === 503 ? 'warning' : 'error',
        text: status === 503
          ? '索引尚未就绪：' + (error.response?.data?.error || '请先构建索引')
          : '检索失败：' + (error.response?.data?.error || error.message)
      });
    } finally {
      if (seq === searchSeqRef.current) setSearching(false);
    }
  };

  const handleSearchClear = () => {
    searchSeqRef.current += 1; // 让在途请求失效
    setSearchKeyword('');
    setSearchResults([]);
    setSearchTotal(0);
    setSearchTookMs(0);
    setSearchPage(1);
    setSearchExecuted(false);
    setSearchError(null);
    setSearchRelaxed(false);
    setSearchMatchedTerms([]);
    setSearchTruncated(false);
    setSearching(false);
  };

  const handleOpenSearchResult = async (hit) => {
    try {
      const { data } = await axios.get(`/api/documents/${hit.doc_id}`);
      setSearchDrawerVisible(false);
      handlePreviewMarkdown({
        id: hit.doc_id,
        filename: hit.filename || data.filename,
        status: data.status
      });
    } catch (error) {
      message.error('打开文档失败：' + (error.response?.data?.error || error.message));
    }
  };

  // 加载指定序号的原始 JSON 内容
  const loadPreviewJson = async (index) => {
    setJsonIndex(index);
    setJsonContent('');
    setJsonLoading(true);
    try {
      const { data } = await axios.get(`/api/documents/${previewDocId}/json/${index}`);
      setJsonContent(data.content || '（JSON 内容为空）');
    } catch (error) {
      message.error('加载JSON失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setJsonLoading(false);
    }
  };

  // 打开重命名弹窗：默认填入当前文件名（去掉 .pdf 扩展名）
  const openRenameModal = (doc) => {
    setRenameDoc(doc);
    setRenameValue(doc.filename.replace(/\.pdf$/i, ''));
    setRenameVisible(true);
  };

  const handleRename = async () => {
    const name = renameValue.trim();
    if (!name) {
      message.warning('文件名不能为空');
      return;
    }
    if (/[\\/:*?"<>|]/.test(name)) {
      message.warning('文件名不能包含 \\ / : * ? " < > | 字符');
      return;
    }
    setRenameLoading(true);
    try {
      await axios.put(`/api/documents/${renameDoc.id}/rename`, { filename: name });
      message.success('重命名成功，PDF 与 Markdown 文件已同步更新');
      setRenameVisible(false);
      setRenameDoc(null);
      fetchDocuments(true);
    } catch (error) {
      message.error('重命名失败：' + (error.response?.data?.error || '未知错误'));
    } finally {
      setRenameLoading(false);
    }
  };

  const columns = [
    {
      title: (
        <Checkbox
          checked={documents.length > 0 && documents.every(doc => selectedRows.includes(doc.id))}
          onChange={(e) => {
            if (e.target.checked) {
              // 跨页保留已选项；本页全选只追加当前页文档，不影响其他页已选
              setSelectedRows(prev => Array.from(new Set([...prev, ...documents.map(doc => doc.id)])));
            } else {
              const visibleIds = new Set(documents.map(doc => doc.id));
              setSelectedRows(prev => prev.filter(id => !visibleIds.has(id)));
            }
          }}
        />
      ),
      dataIndex: 'id',
      key: 'id',
      width: 48,
      align: 'center',
      render: (id) => (
        liteCells ? (
          <input
            type="checkbox"
            className="pm-lite-check"
            checked={selectedRows.includes(id)}
            onChange={(e) => {
              if (e.target.checked) {
                setSelectedRows([...selectedRows, id]);
              } else {
                setSelectedRows(selectedRows.filter(rowId => rowId !== id));
              }
            }}
          />
        ) : (
        <Checkbox
          checked={selectedRows.includes(id)}
          onChange={(e) => {
            if (e.target.checked) {
              setSelectedRows([...selectedRows, id]);
            } else {
              setSelectedRows(selectedRows.filter(rowId => rowId !== id));
            }
          }}
        />
        )
      )
    },
    {
      title: resizableTitle('文件名', 'filename'),
      dataIndex: 'filename',
      key: 'filename',
      width: colWidths.filename,
      sorter: true,
      sortOrder: sortState.field === 'filename' ? sortState.order : null,
      render: (filename, record) => (
        <div className="pm-file-cell">
          {record.markdown_path && (
            liteCells ? (
              <EyeOutlined
                title="预览 Markdown"
                style={{ color: 'var(--pm-primary)', flex: '0 0 auto', cursor: 'pointer' }}
                onClick={(e) => { e.stopPropagation(); handlePreviewMarkdown(record); }}
              />
            ) : (
            <Tooltip title="预览 Markdown">
              <EyeOutlined
                style={{ color: 'var(--pm-primary)', flex: '0 0 auto', cursor: 'pointer' }}
                onClick={(e) => { e.stopPropagation(); handlePreviewMarkdown(record); }}
              />
            </Tooltip>
            )
          )}
          <div className="pm-file-main">
            {liteCells ? (
              <>
                <span
                  className="pm-file-name"
                  title={filename}
                  onClick={() => openRenameModal(record)}
                >
                  {filename}
                </span>
                {!showPathColumn && (
                  <span className="pm-file-path" title={record.original_path || '—'}>
                    {record.original_path || '—'}
                  </span>
                )}
              </>
            ) : (
            <>
            <Tooltip
              title={(
                <div>
                  <div style={{ wordBreak: 'break-all' }}>{filename}</div>
                  <div style={{ opacity: 0.7 }}>点击重命名</div>
                </div>
              )}
            >
              <span className="pm-file-name" onClick={() => openRenameModal(record)}>
                {filename}
              </span>
            </Tooltip>
            {!showPathColumn && (
              <Tooltip title={record.original_path || '—'}>
                <span className="pm-file-path">{record.original_path || '—'}</span>
              </Tooltip>
            )}
            </>
            )}
          </div>
        </div>
      )
    },
    {
      title: resizableTitle('原始路径', 'original_path'),
      dataIndex: 'original_path',
      key: 'original_path',
      width: colWidths.original_path,
      hidden: !showPathColumn,
      ellipsis: true,
      sorter: true,
      sortOrder: sortState.field === 'original_path' ? sortState.order : null,
      render: (path, record) => {
        const isEditing = editingCell?.docId === record.id && editingCell?.field === 'original_path';
        if (isEditing) {
          return (
            <Input.TextArea
              autoFocus
              value={editValue}
              onChange={(e) => setEditValue(e.target.value)}
              onPressEnter={(e) => { e.preventDefault(); saveInlineMove(record); }}
              onBlur={() => saveInlineMove(record)}
              onKeyDown={(e) => { if (e.key === 'Escape') cancelEdit(); }}
              autoSize={{ minRows: 1, maxRows: 3 }}
              loading={editSaving}
              placeholder="新的完整路径（含文件名.pdf）"
            />
          );
        }
        if (liteCells) {
          return (
            <span
              className="pm-file-name"
              title={path}
              onClick={() => startEdit(record, 'original_path', path || '')}
              style={{ color: 'var(--pm-primary)' }}
            >
              {path || '—'}
            </span>
          );
        }
        return (
          <Tooltip title={path}>
            <span
              onClick={() => startEdit(record, 'original_path', path || '')}
              style={{ cursor: 'pointer', color: 'var(--pm-primary)' }}
            >
              {path || '—'}
            </span>
          </Tooltip>
        );
      }
    },
    {
      title: '大小',
      dataIndex: 'file_size',
      key: 'file_size',
      width: 96,
      sorter: true,
      sortOrder: sortState.field === 'file_size' ? sortState.order : null,
      render: (size) => {
        if (size < 1024) return `${size} B`;
        if (size < 1024 * 1024) return `${(size / 1024).toFixed(2)} KB`;
        return `${(size / (1024 * 1024)).toFixed(2)} MB`;
      }
    },
    {
      title: '页数',
      dataIndex: 'page_count',
      key: 'page_count',
      width: 76,
      sorter: true,
      sortOrder: sortState.field === 'page_count' ? sortState.order : null
    },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 104,
      sorter: true,
      sortOrder: sortState.field === 'status' ? sortState.order : null,
      render: (status, record) => {
        const meta = DOC_STATUS[status] || { label: status, tone: 'neutral' };
        const tone = STATUS_TONES[meta.tone] || STATUS_TONES.neutral;
        if (liteCells) {
          return (
            <span
              className="pm-status-pill"
              title={status === 'failed' && record.error_message ? record.error_message : meta.label}
              style={{ color: tone.color, background: tone.bg, borderColor: tone.border }}
            >
              {meta.label}
            </span>
          );
        }
        const tag = <Tag style={statusTagStyle(meta.tone)}>{meta.label}</Tag>;
        // 失败时悬停展示后端返回的具体错误原因
        if (status === 'failed' && record.error_message) {
          return <Tooltip title={record.error_message}>{tag}</Tooltip>;
        }
        return tag;
      }
    },
    {
      title: 'OCR',
      dataIndex: 'ocr_disabled',
      key: 'ocr_disabled',
      width: 76,
      sorter: true,
      sortOrder: sortState.field === 'ocr_disabled' ? sortState.order : null,
      render: (ocrDisabled, record) => (
        liteCells ? (
          <input
            type="checkbox"
            className="pm-lite-check"
            checked={!ocrDisabled}
            disabled={record.status === 'splitting' || record.status === 'ocr_processing'}
            title={ocrDisabled ? '已禁止OCR（点击开启）' : '允许OCR（点击禁止）'}
            onChange={(e) => handleToggleOcrDisabled(record, !e.target.checked)}
          />
        ) : (
        <Tooltip title={ocrDisabled ? '已禁止OCR（点击开启）' : '允许OCR（点击禁止）'}>
          <Switch
            size="small"
            checked={!ocrDisabled}
            loading={ocrSwitchLoadingIds.includes(record.id)}
            disabled={record.status === 'splitting' || record.status === 'ocr_processing'}
            onChange={(checked) => handleToggleOcrDisabled(record, !checked)}
          />
        </Tooltip>
        )
      )
    },
    {
      title: '创建时间',
      dataIndex: 'created_at',
      key: 'created_at',
      width: 160,
      hidden: !showCreatedColumn,
      sorter: true,
      sortOrder: sortState.field === 'created_at' ? sortState.order : null,
      render: (v) => v ? new Date(v).toLocaleString('zh-CN', { hour12: false }) : '—'
    },
    {
      title: '更新时间',
      dataIndex: 'updated_at',
      key: 'updated_at',
      width: 160,
      sorter: true,
      sortOrder: sortState.field === 'updated_at' ? sortState.order : null,
      render: (v) => v ? new Date(v).toLocaleString('zh-CN', { hour12: false }) : '—'
    },
    {
      title: '点击',
      dataIndex: 'click_count',
      key: 'click_count',
      width: 72,
      align: 'center',
      hidden: !showClickColumn,
      sorter: true,
      sortOrder: sortState.field === 'click_count' ? sortState.order : null,
      render: (v) => v || 0
    },
    {
      title: '评分',
      dataIndex: 'rating',
      key: 'rating',
      width: 96,
      sorter: true,
      sortOrder: sortState.field === 'rating' ? sortState.order : null,
      render: (v, record) => (
        liteCells ? (
          <select
            className="pm-lite-select"
            value={v || 0}
            disabled={ratingSavingIds.includes(record.id)}
            onChange={(e) => handleSetRating(record, Number(e.target.value))}
          >
            <option value={0}>未评分</option>
            {Array.from({ length: 10 }, (_, i) => (
              <option key={i + 1} value={i + 1}>{i + 1} 分</option>
            ))}
          </select>
        ) : (
        <Select
          size="small"
          variant="borderless"
          style={{ width: 82 }}
          value={v || 0}
          loading={ratingSavingIds.includes(record.id)}
          onChange={(value) => handleSetRating(record, value)}
          options={[
            { value: 0, label: '未评分' },
            ...Array.from({ length: 10 }, (_, i) => ({ value: i + 1, label: `${i + 1} 分` }))
          ]}
        />
        )
      )
    },
    {
      title: '标签',
      dataIndex: 'tags',
      key: 'tags',
      width: 200,
      render: (docTags, record) => {
        const isEditing = editingCell?.docId === record.id && editingCell?.field === 'tags';
        if (isEditing) {
          // 可选标签 = 当前文档标签 ∪ 全部已有标签；mode="tags" 支持搜索与输入新建
          const optionMap = new Map();
          (docTags || []).forEach(t => optionMap.set(t.name, t.color));
          tags.forEach(t => { if (!optionMap.has(t.name)) optionMap.set(t.name, t.color); });
          return (
            <Select
              mode="tags"
              autoFocus
              open
              style={{ width: '100%', minWidth: 200 }}
              value={editValue}
              tokenSeparators={[',', '，']}
              options={[...optionMap.entries()].map(([name, color]) => ({
                value: name,
                label: (
                  <span>
                    <span style={{ display: 'inline-block', width: 8, height: 8, borderRadius: '50%', background: color, marginRight: 8 }} />
                    {name}
                  </span>
                )
              }))}
              onChange={(values) => setEditValue(values)}
              onBlur={() => saveInlineTags(record, editValue)}
              onKeyDown={(e) => { if (e.key === 'Escape') cancelEdit(); }}
              placeholder="搜索或输入标签，失焦保存"
              loading={editSaving}
            />
          );
        }
        if (liteCells) {
          return (
            <div
              onClick={() => startEdit(record, 'tags', (docTags || []).map(t => t.name))}
              style={{ cursor: 'pointer', minHeight: 22, overflow: 'hidden', whiteSpace: 'nowrap', textOverflow: 'ellipsis' }}
              title={(docTags || []).map(t => t.name).join('、') || '点击添加标签'}
            >
              {(docTags || []).length === 0 ? (
                <span style={{ color: 'var(--pm-text-disabled)' }}>点击添加标签</span>
              ) : (
                docTags.map(tag => (
                  <span key={tag.id} className="pm-tag-chip" style={{ borderColor: tag.color, color: tag.color }}>
                    {tag.name}
                  </span>
                ))
              )}
            </div>
          );
        }
        return (
          <Tooltip title="点击编辑标签">
            <div
              onClick={() => startEdit(record, 'tags', (docTags || []).map(t => t.name))}
              style={{ cursor: 'pointer', minHeight: 22 }}
            >
              {(docTags || []).length === 0 ? (
                <span style={{ color: 'var(--pm-text-disabled)' }}>点击添加标签</span>
              ) : (
                <Space size={4} wrap>
                  {docTags.map(tag => (
                    <Tag
                      key={tag.id}
                      color={tag.color}
                      closable
                      onClose={(e) => { e.stopPropagation(); handleRemoveTag(record.id, tag.id); }}
                    >
                      {tag.name}
                    </Tag>
                  ))}
                </Space>
              )}
            </div>
          </Tooltip>
        );
      }
    },
    {
      title: '操作',
      key: 'actions',
      width: 156,
      fixed: 'right',
      render: (_, record) => {
        // 处理中状态以服务端为准（刷新页面后依然正确）；ocrLoadingIds 仅覆盖提交请求的在途瞬间
        const isProcessing = record.status === 'splitting' || record.status === 'ocr_processing';
        const isSubmitting = ocrLoadingIds.includes(record.id);
        const isRebuilding = rebuildLoadingIds.includes(record.id);
        const hasJson = (record.json_paths || []).length > 0;
        if (liteCells) {
          return (
            <div className="pm-lite-actions">
              <button
                type="button"
                className="pm-lite-btn pm-lite-btn-primary"
                title="OCR 识别"
                onClick={() => handleOCR(record.id)}
                disabled={isProcessing || isSubmitting}
              >
                <ScanOutlined />
              </button>
              <button
                type="button"
                className="pm-lite-btn"
                title={hasJson ? '从本地 JSON 重建 Markdown（含表格）' : '没有可用的 OCR JSON'}
                onClick={() => handleRebuildMarkdown(record)}
                disabled={!hasJson || isProcessing || isRebuilding}
              >
                <SyncOutlined />
              </button>
              {record.markdown_path && (
                <button
                  type="button"
                  className="pm-lite-btn"
                  title="复制 Markdown 路径"
                  onClick={() => handleCopyMarkdownPath(record.markdown_path)}
                >
                  <CopyOutlined />
                </button>
              )}
              <button
                type="button"
                className="pm-lite-btn pm-lite-btn-danger"
                title="删除"
                onClick={() => handleDelete(record.id)}
              >
                <DeleteOutlined />
              </button>
            </div>
          );
        }
        const iconBtn = (title, node) => (
          <Tooltip title={title} placement="top">
            {node}
          </Tooltip>
        );
        return (
        <Space size={2} style={{ flexWrap: 'nowrap' }}>
          {iconBtn('OCR 识别',
            <Button
              type="primary"
              size="small"
              icon={<ScanOutlined />}
              onClick={() => handleOCR(record.id)}
              disabled={isProcessing || isSubmitting}
              loading={isProcessing || isSubmitting}
            />
          )}
          {iconBtn(hasJson ? '从本地 JSON 重建 Markdown（含表格）' : '没有可用的 OCR JSON',
            <Button
              size="small"
              type="text"
              icon={<SyncOutlined />}
              onClick={() => handleRebuildMarkdown(record)}
              disabled={!hasJson || isProcessing || isRebuilding}
              loading={isRebuilding}
            />
          )}
          {record.markdown_path && iconBtn('复制 Markdown 路径',
            <Button
              size="small"
              type="text"
              icon={<CopyOutlined />}
              onClick={() => handleCopyMarkdownPath(record.markdown_path)}
            />
          )}
          {iconBtn('删除',
            <Button
              size="small"
              type="text"
              danger
              icon={<DeleteOutlined />}
              onClick={() => handleDelete(record.id)}
            />
          )}
        </Space>
        );
      }
    }
  ].filter((column) => !column.hidden);

  // 检索抽屉的派生状态：构建中禁用按钮但旧索引仍可检索
  const searchStatus = searchIndexStatus;
  const searchReady = !!searchStatus?.ready;
  const searchBuilding = !!searchStatus?.running;
  const searchBuildDisabled = searchBuilding || searchBuildLoading;
  const searchProgressPercent = searchStatus?.total > 0
    ? Math.min(100, Math.floor((searchStatus.processed / searchStatus.total) * 100))
    : 0;
  const searchEmptyDescription = !searchReady
    ? '索引尚未就绪，请先构建索引'
    : (searchStatus?.doc_count === 0
        ? '索引中还没有文档，请先执行全量重建'
        : searchMode === 'exact'
          ? '没有文档包含该完整字符串。可以试试：缩短字符串、去掉标点，或切回「智能」模式按分词检索'
          : '没有找到匹配的内容，试试更短的关键词');

  // 窄窗口把批量操作收进下拉菜单，避免工具条换行挤压
  const hasSelection = selectedRows.length > 0;
  const batchMenuItems = [
    { key: 'ocr', icon: <ScanOutlined />, label: '批量 OCR', disabled: !hasSelection || batchLoading, onClick: handleBatchOCR },
    { key: 'rebuild', icon: <SyncOutlined />, label: '批量重建 Markdown', disabled: !hasSelection || batchLoading, onClick: handleBatchRebuildMarkdown },
    { key: 'tag', icon: <TagOutlined />, label: '批量添加标签', disabled: !hasSelection, onClick: openBatchTagModal },
    { key: 'rename', icon: <EditOutlined />, label: '批量重命名', disabled: !hasSelection, onClick: openBatchRenameModal },
    { key: 'enable-ocr', icon: <CheckCircleOutlined />, label: '批量启用 OCR', disabled: !hasSelection || batchLoading, onClick: () => handleBatchOcrDisabled(false) },
    { key: 'disable-ocr', icon: <PauseCircleOutlined />, label: '批量禁用 OCR', disabled: !hasSelection || batchLoading, onClick: () => handleBatchOcrDisabled(true) },
    { key: 'rating', icon: <StarOutlined />, label: '批量打分', disabled: !hasSelection, onClick: () => { setBatchRatingValue(10); setBatchRatingVisible(true); } },
    { type: 'divider' },
    { key: 'delete', danger: true, icon: <DeleteOutlined />, label: '批量删除', disabled: !hasSelection || batchLoading, onClick: handleBatchDelete }
  ];
  const schedulerActive = schedulerStatus.running || schedulerStatus.enabled;
  const schedulerLabel = schedulerActive
    ? (isNarrow
      ? `自动OCR中 ${schedulerStatus.active_workers}/${schedulerStatus.max_workers}·剩${schedulerStatus.pending_count}`
      : `全量OCR中 (${schedulerStatus.active_workers}/${schedulerStatus.max_workers}并发, 剩余${schedulerStatus.pending_count})`)
    : (isNarrow
      ? `自动OCR (${schedulerStatus.pending_count})`
      : `开启全量自动OCR (${schedulerStatus.pending_count}个待处理)`);

  return (
    <Layout className="pm-layout" style={{ height: '100vh' }}>
      <Header className="pm-header">
        <h1 className="pm-app-title">
          {isCompact ? 'PDF 管理' : 'PDF 文件管理系统'}
          {!isCompact && <span className="pm-title-sub">Markdown · OCR · 检索</span>}
        </h1>
        <div className="pm-header-tools">
          {headerIconOnly ? (
            <Tooltip title="刷新列表">
              <Button icon={<ReloadOutlined />} onClick={fetchDocuments} loading={tableLoading} />
            </Tooltip>
          ) : (
            <Button icon={<ReloadOutlined />} onClick={fetchDocuments} loading={tableLoading}>
              刷新
            </Button>
          )}
          <Button type="primary" icon={<FolderOpenOutlined />} onClick={openScanModal}>
            {isCompact ? null : '扫描目录'}
          </Button>
          <Upload
            accept=".pdf"
            showUploadList={false}
            customRequest={handleUpload}
          >
            <Button icon={<UploadOutlined />} loading={uploading} title="上传 PDF">
              {isCompact ? null : '上传 PDF'}
            </Button>
          </Upload>
          {isNarrow ? (
            <Dropdown menu={{ items: batchMenuItems }} trigger={['click']}>
              <Button icon={<MoreOutlined />}>
                批量操作{hasSelection ? ` (${selectedRows.length})` : ''}
              </Button>
            </Dropdown>
          ) : (
            <>
              <Button
                type="primary"
                icon={<ScanOutlined />}
                onClick={handleBatchOCR}
                disabled={!hasSelection}
                loading={batchLoading}
              >
                批量OCR
              </Button>
              <Button
                icon={<SyncOutlined />}
                onClick={handleBatchRebuildMarkdown}
                disabled={!hasSelection}
                loading={batchLoading}
              >
                批量重建
              </Button>
              <Tooltip title="批量启用 OCR">
                <Button
                  icon={<CheckCircleOutlined />}
                  onClick={() => handleBatchOcrDisabled(false)}
                  disabled={!hasSelection}
                  loading={batchLoading}
                >
                  {viewportWidth >= 1920 ? '启用OCR' : null}
                </Button>
              </Tooltip>
              <Tooltip title="批量禁用 OCR">
                <Button
                  icon={<PauseCircleOutlined />}
                  onClick={() => handleBatchOcrDisabled(true)}
                  disabled={!hasSelection}
                  loading={batchLoading}
                >
                  {viewportWidth >= 1920 ? '禁用OCR' : null}
                </Button>
              </Tooltip>
              <Tooltip title="批量打分">
                <Button
                  icon={<StarOutlined />}
                  onClick={() => { setBatchRatingValue(10); setBatchRatingVisible(true); }}
                  disabled={!hasSelection}
                >
                  {viewportWidth >= 1920 ? '批量打分' : null}
                </Button>
              </Tooltip>
              <Button
                danger
                icon={<DeleteOutlined />}
                onClick={handleBatchDelete}
                disabled={!hasSelection}
                loading={batchLoading}
              >
                批量删除
              </Button>
              <Button
                icon={<TagOutlined />}
                onClick={openBatchTagModal}
                disabled={!hasSelection}
              >
                批量添加标签
              </Button>
              <Button
                icon={<EditOutlined />}
                onClick={openBatchRenameModal}
                disabled={!hasSelection}
              >
                批量重命名
              </Button>
            </>
          )}
          <Tooltip title={
            schedulerActive
              ? "自动调度运行中：后台按空闲并发持续从数据库拉取待OCR文档，断电重启自动恢复。点击可暂停。"
              : "开启后，后台将自动按并发限制依次处理所有待OCR文件（无需人工逐页翻选），服务重启也会自动续跑。"
          }>
            <Button
              type={schedulerActive ? 'dashed' : 'default'}
              danger={schedulerActive}
              icon={schedulerActive ? <PauseCircleOutlined /> : <PlayCircleOutlined />}
              onClick={handleToggleScheduler}
              loading={schedulerBtnLoading}
              style={schedulerStatus.running ? {
                borderColor: 'var(--pm-success)',
                color: 'var(--pm-success)'
              } : undefined}
            >
              {schedulerLabel}
            </Button>
          </Tooltip>
          {headerIconOnly ? (
            <Tooltip title="全文检索">
              <Button icon={<SearchOutlined />} onClick={openSearchDrawer} />
            </Tooltip>
          ) : (
            <Button icon={<SearchOutlined />} onClick={openSearchDrawer}>
              全文检索
            </Button>
          )}
          {headerIconOnly ? (
            <Tooltip title="系统设置">
              <Button icon={<SettingOutlined />} onClick={openSettings} />
            </Tooltip>
          ) : (
            <Button icon={<SettingOutlined />} onClick={openSettings}>
              设置
            </Button>
          )}
        </div>
      </Header>
      <Content className="pm-content" style={{ padding: contentPadding }}>
        <div className="pm-page">
        <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 8 }}>
          <Button
            size="small"
            icon={<ReloadOutlined />}
            onClick={handleRefreshStats}
            loading={statsRefreshing}
          >
            刷新统计
          </Button>
        </div>
        <Row gutter={[16, 16]} style={{ marginBottom: 16 }}>
          <Col xs={24} sm={8}>
            <Card size="small" style={{ borderRadius: 8, boxShadow: 'var(--pm-shadow-card)', borderColor: 'var(--pm-border-light)' }}>
              <Statistic
                title={<span style={{ color: 'var(--pm-text-2)', fontWeight: 500 }}>已成功识别 (OCR完成)</span>}
                value={schedulerStatus.completed_count || 0}
                valueStyle={{ color: 'var(--pm-success)', fontWeight: 600 }}
                prefix={<CheckCircleOutlined style={{ marginRight: 4 }} />}
                suffix={<span style={{ fontSize: 13, color: 'var(--pm-text-3)' }}>本</span>}
              />
            </Card>
          </Col>
          <Col xs={24} sm={8}>
            <Card size="small" style={{ borderRadius: 8, boxShadow: 'var(--pm-shadow-card)', borderColor: 'var(--pm-border-light)' }}>
              <Statistic
                title={<span style={{ color: 'var(--pm-text-2)', fontWeight: 500 }}>剩余待识别 (排队中)</span>}
                value={schedulerStatus.pending_count || 0}
                valueStyle={{ color: 'var(--pm-primary)', fontWeight: 600 }}
                prefix={<ClockCircleOutlined style={{ marginRight: 4 }} />}
                suffix={<span style={{ fontSize: 13, color: 'var(--pm-text-3)' }}>本</span>}
              />
            </Card>
          </Col>
          <Col xs={24} sm={8}>
            <Card size="small" style={{ borderRadius: 8, boxShadow: 'var(--pm-shadow-card)', borderColor: 'var(--pm-border-light)' }}>
              <Statistic
                title={<span style={{ color: 'var(--pm-text-2)', fontWeight: 500 }}>已提交MinerU / 处理中</span>}
                value={schedulerStatus.processing_count || 0}
                valueStyle={{ color: 'var(--pm-warning)', fontWeight: 600 }}
                prefix={<SyncOutlined spin={Boolean(schedulerStatus.processing_count)} style={{ marginRight: 4 }} />}
                suffix={<span style={{ fontSize: 13, color: 'var(--pm-text-3)' }}>本 ({schedulerStatus.active_batches || 0}批次)</span>}
              />
            </Card>
          </Col>
        </Row>

        <div className="pm-filter-bar">
          <Input
            allowClear
            prefix={<SearchOutlined style={{ color: 'var(--pm-text-disabled)' }} />}
            placeholder="输入文件名或标签，按回车搜索"
            value={searchText}
            onChange={handleSearchChange}
            onPressEnter={handleSearch}
            style={{ flex: '1 1 260px', maxWidth: 420, minWidth: 200 }}
          />
          <Input
            allowClear
            prefix={<FolderOutlined style={{ color: 'var(--pm-text-disabled)' }} />}
            placeholder="按原始路径搜索，回车生效"
            value={pathKeyword}
            onChange={handlePathChange}
            onPressEnter={handlePathSearch}
            style={{ flex: '1 1 220px', maxWidth: 360, minWidth: 180 }}
          />
          <Select
            mode="multiple"
            allowClear
            placeholder="按状态过滤"
            value={statusFilter}
            onChange={handleStatusFilterChange}
            options={Object.entries(DOC_STATUS).map(([value, meta]) => ({
              value,
              label: (
                <span>
                  <span style={{
                    display: 'inline-block',
                    width: 8,
                    height: 8,
                    borderRadius: '50%',
                    background: STATUS_TONES[meta.tone].color,
                    marginRight: 8
                  }} />
                  {meta.label}
                </span>
              )
            }))}
            style={{ flex: '0 1 220px', minWidth: 170 }}
            maxTagCount="responsive"
          />
          <Select
            mode="multiple"
            allowClear
            placeholder="按标签过滤"
            value={tagFilter}
            onChange={handleTagFilterChange}
            options={tags.map(tag => ({
              value: tag.id,
              label: (
                <span>
                  <span style={{ display: 'inline-block', width: 8, height: 8, borderRadius: '50%', background: tag.color, marginRight: 8 }} />
                  {tag.name}
                </span>
              )
            }))}
            style={{ flex: '0 1 220px', minWidth: 170 }}
            maxTagCount="responsive"
            notFoundContent="暂无标签"
          />
          <Select
            allowClear
            placeholder="OCR开关过滤"
            value={ocrDisabledFilter}
            onChange={handleOcrDisabledFilterChange}
            options={[
              { value: false, label: '允许OCR' },
              { value: true, label: '禁止OCR' }
            ]}
            style={{ flex: '0 1 160px', minWidth: 140 }}
          />
        </div>
        {selectedRows.length > 0 && (
          <div className="pm-selection-bar">
            <span>已选 {selectedRows.length} 项</span>
            <span style={{ color: 'var(--pm-text-3)' }}>
              {isNarrow ? '批量操作在顶部「批量操作」菜单中' : '批量操作在顶部工具栏'}
            </span>
            <Button size="small" type="link" onClick={() => setSelectedRows([])} style={{ marginLeft: 'auto' }}>
              清空选择
            </Button>
          </div>
        )}
        <div className="pm-list">
          <div className="pm-table-area" ref={tableAreaRef}>
        <Table
          dataSource={documents}
          columns={columns}
          rowKey="id"
          bordered
          size={isCompact || liteCells ? 'small' : 'middle'}
          tableLayout="fixed"
          scroll={{ x: tableScrollX, y: tableBodyHeight }}
          loading={liteCells ? { spinning: tableLoading, tip: `正在加载并渲染 ${pagination.pageSize} 条记录，请稍候…` } : tableLoading}
          onChange={(_pagination, _filters, sorter, extra) => {
            // 翻页/改页大小由 pagination.onChange 处理，这里只响应排序变化；
            // 否则翻页会触发本回调并强制 page:1，导致永远回到第一页
            if (extra.action !== 'sort') return;
            // 服务端排序：sorter 可能是对象或数组，取首个有效项
            const s = Array.isArray(sorter) ? sorter[0] : sorter;
            if (!s || !s.field || !s.order) {
              // 取消排序：回退到默认 updated_at desc
              setSortState({ field: 'updated_at', order: 'descend' });
              fetchDocuments(false, { page: 1, sort_by: 'updated_at', sort_order: 'desc' });
              return;
            }
            const order = s.order === 'ascend' ? 'asc' : 'desc';
            setSortState({ field: s.field, order: s.order });
            fetchDocuments(false, { page: 1, sort_by: s.field, sort_order: order });
          }}
          pagination={{
            current: pagination.current,
            pageSize: pagination.pageSize,
            total,
            showSizeChanger: true,
            pageSizeOptions: [10, 20, 50, 100, 1000],
            showLessItems: isCompact,
            showQuickJumper: isWide,
            size: isCompact ? 'small' : 'default',
            responsive: true,
            showTotal: (t) => `共 ${t} 个文档`,
            onChange: (page, pageSize) => {
              setPagination({ current: page, pageSize });
              fetchDocuments(false, { page, pageSize });
            }
          }}
        />
          </div>
        </div>
        </div>
      </Content>

      {/* 批量打分弹窗 */}
      <Modal
        title={`批量打分（已选 ${selectedRows.length} 个文档）`}
        open={batchRatingVisible}
        onCancel={() => setBatchRatingVisible(false)}
        onOk={handleBatchSetRating}
        confirmLoading={batchRatingLoading}
        okText="保存"
        cancelText="取消"
        width={Math.min(420, viewportWidth - 32)}
        destroyOnHidden
      >
        <Select
          style={{ width: '100%' }}
          value={batchRatingValue}
          onChange={setBatchRatingValue}
          options={[
            { value: 0, label: '未评分（清除）' },
            ...Array.from({ length: 10 }, (_, i) => ({ value: i + 1, label: `${i + 1} 分` }))
          ]}
        />
        <div style={{ marginTop: 8, color: 'var(--pm-text-3)', fontSize: '12px' }}>
          评分范围 0~10，0 表示未评分；将覆盖所选文档的原有评分。
        </div>
      </Modal>

      {/* 批量添加标签弹窗 */}
      <Modal
        title={`批量添加标签（已选 ${selectedRows.length} 个文档）`}
        open={isBatchTagModalVisible}
        onCancel={() => setIsBatchTagModalVisible(false)}
        onOk={handleBatchAddTags}
        confirmLoading={batchTagLoading}
        okText="添加"
        cancelText="取消"
        width={Math.min(520, viewportWidth - 32)}
        destroyOnHidden
      >
        <Select
          mode="tags"
          style={{ width: '100%' }}
          placeholder="搜索选择已有标签，或输入新标签后回车"
          value={batchTagNames}
          onChange={setBatchTagNames}
          tokenSeparators={[',', '，']}
          options={tags.map(tag => ({
            value: tag.name,
            label: (
              <span>
                <span style={{ display: 'inline-block', width: 8, height: 8, borderRadius: '50%', background: tag.color, marginRight: 8 }} />
                {tag.name}
              </span>
            )
          }))}
          notFoundContent="暂无标签，可直接输入新标签名"
        />
        <div style={{ marginTop: 8, color: 'var(--pm-text-3)', fontSize: '12px' }}>
          已拥有所选标签的文档会自动去重，不会重复添加。
        </div>
      </Modal>

      {/* 批量重命名弹窗：规则 → 预览 → 执行结果 */}
      <Modal
        title={`批量重命名（已选 ${renameTargetCount} 个文档）`}
        open={isBatchRenameVisible}
        onCancel={closeBatchRename}
        width={Math.min(920, viewportWidth - 32)}
        destroyOnHidden
        footer={renameResult ? (
          <Button type="primary" onClick={closeBatchRename}>完成</Button>
        ) : (
          <>
            <Button onClick={closeBatchRename}>取消</Button>
            <Button onClick={handleRenamePreview} loading={renamePreviewLoading}>预览</Button>
            <Button
              type="primary"
              onClick={handleRenameExecute}
              loading={renameExecLoading}
              disabled={!(renamePreview?.summary?.ok > 0)}
            >
              执行重命名
            </Button>
          </>
        )}
      >
        {!renameResult && (
          <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
            <Select
              size="small"
              style={{ width: 88 }}
              value={renameForm.mode}
              onChange={(mode) => setRenameForm((f) => ({ ...f, mode }))}
              options={[
                { value: 'literal', label: '字面' },
                { value: 'regex', label: '正则' }
              ]}
            />
            <Input
              size="small"
              allowClear
              style={{ flex: '1 1 220px', minWidth: 150 }}
              placeholder="查找内容"
              value={renameForm.find}
              onChange={(e) => setRenameForm((f) => ({ ...f, find: e.target.value }))}
            />
            <span style={{ color: 'var(--pm-text-3)' }}>→</span>
            <Input
              size="small"
              allowClear
              style={{ flex: '1 1 220px', minWidth: 150 }}
              placeholder="替换为，支持 $1 / $<name> / $& / $$"
              value={renameForm.replace}
              onChange={(e) => setRenameForm((f) => ({ ...f, replace: e.target.value }))}
            />
          </div>
        )}
        {!renameResult && (
          <div style={{ display: 'flex', gap: 20, alignItems: 'center', flexWrap: 'wrap', marginTop: 10 }}>
            <span style={{ fontSize: 13, color: 'var(--pm-text-2)' }}>
              <Switch
                size="small"
                checked={renameForm.caseSensitive}
                onChange={(v) => setRenameForm((f) => ({ ...f, caseSensitive: v }))}
              />
              <span style={{ marginLeft: 6 }}>大小写敏感</span>
            </span>
            <span style={{ fontSize: 13, color: 'var(--pm-text-2)' }}>
              <Switch
                size="small"
                checked={renameForm.mainNameOnly}
                onChange={(v) => setRenameForm((f) => ({ ...f, mainNameOnly: v }))}
              />
              <span style={{ marginLeft: 6 }}>只改主名（扩展名固定保留 .pdf）</span>
            </span>
            <span style={{ fontSize: 13, color: 'var(--pm-text-2)' }}>
              <Switch
                size="small"
                checked={renameForm.cleanWhitespace}
                onChange={(v) => setRenameForm((f) => ({ ...f, cleanWhitespace: v }))}
              />
              <span style={{ marginLeft: 6 }}>清理首尾空格与重复空格</span>
            </span>
          </div>
        )}
        {renameError && (
          <div style={{ marginTop: 8, fontSize: 12, color: 'var(--pm-error)' }}>重命名失败：{renameError}</div>
        )}

        {renameResult && (
          <Alert
            type={renameResult.summary?.failed ? 'warning' : 'success'}
            showIcon
            style={{ marginBottom: 12 }}
            message={`重命名完成：成功 ${renameResult.summary?.success ?? 0} 个，失败 ${renameResult.summary?.failed ?? 0} 个，跳过 ${renameResult.summary?.skipped ?? 0} 条${[
              renameResult.summary?.skipped_conflict ? `冲突 ${renameResult.summary.skipped_conflict}` : '',
              renameResult.summary?.skipped_invalid ? `非法 ${renameResult.summary.skipped_invalid}` : '',
              renameResult.summary?.skipped_missing ? `不存在 ${renameResult.summary.skipped_missing}` : '',
              renameResult.summary?.skipped_readonly ? `只读 ${renameResult.summary.skipped_readonly}` : '',
              renameResult.summary?.skipped_unchanged ? `无变化 ${renameResult.summary.skipped_unchanged}` : ''
            ].filter(Boolean).length
              ? `（${[renameResult.summary?.skipped_conflict ? `冲突 ${renameResult.summary.skipped_conflict}` : '',
                  renameResult.summary?.skipped_invalid ? `非法 ${renameResult.summary.skipped_invalid}` : '',
                  renameResult.summary?.skipped_missing ? `不存在 ${renameResult.summary.skipped_missing}` : '',
                  renameResult.summary?.skipped_readonly ? `只读 ${renameResult.summary.skipped_readonly}` : '',
                  renameResult.summary?.skipped_unchanged ? `无变化 ${renameResult.summary.skipped_unchanged}` : ''
                ].filter(Boolean).join('，')}）`
              : ''}`}
            description={
              renameResult.index_pending > 0
                ? `检索索引有 ${renameResult.index_pending} 个文档待更新，建议立即执行增量更新，避免检索片段仍指向旧文件名。`
                : undefined
            }
            action={
              renameResult.index_pending > 0 ? (
                <Button size="small" onClick={() => doSearchBuild('incremental')}>增量更新</Button>
              ) : null
            }
          />
        )}

        {renamePreview && !renameResult && (
          <div style={{ marginTop: 12, marginBottom: 8, fontSize: 13, display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
            可改 <b style={{ color: 'var(--pm-success)' }}>{renamePreview.summary?.ok ?? 0}</b> 条
            <span style={{ color: 'var(--pm-text-3)' }}>·</span>
            跳过 <b>{Math.max(0, (renamePreview.summary?.total ?? 0) - (renamePreview.summary?.ok ?? 0) - (renamePreview.summary?.conflict ?? 0))}</b> 条
            <span style={{ color: 'var(--pm-text-3)' }}>·</span>
            <span
              role="button"
              onClick={() => setRenameOnlyConflict((v) => !v)}
              style={{
                color: 'var(--pm-error)',
                cursor: 'pointer',
                textDecoration: 'underline',
                fontWeight: renameOnlyConflict ? 700 : 400
              }}
              title="点击只看冲突行"
            >
              冲突 {renamePreview.summary?.conflict ?? 0}
            </span>
            条
            <Checkbox
              checked={renameOnlyConflict}
              onChange={(e) => setRenameOnlyConflict(e.target.checked)}
            >
              只看冲突
            </Checkbox>
            {(renamePreview.summary?.conflict || 0) > 0 && (
              <span style={{ color: 'var(--pm-text-2)' }}>
                冲突行会在执行时自动跳过，可直接在「新文件名」列手动修改
              </span>
            )}
            {!(renamePreview.summary?.ok > 0) && (
              <span style={{ color: 'var(--pm-error)' }}>没有可执行的行，请调整规则或手动指定新名</span>
            )}
          </div>
        )}

        {(renameResult || renamePreview) && (
          <Table
            size="small"
            bordered={false}
            rowKey={(row) => `rename-${row.doc_id}`}
            dataSource={
              renameResult
                ? renameResult.items
                : (renameOnlyConflict
                  ? (renamePreview.items || []).filter((r) => r.status === 'conflict')
                  : renamePreview.items)
            }
            pagination={false}
            scroll={{ y: 320 }}
            rowClassName={(row) => (row.status === 'conflict' ? 'pm-row-conflict' : '')}
            columns={[
              { title: '原文件名', dataIndex: 'old_filename', width: '28%', ellipsis: true },
              { title: '', width: 30, align: 'center', render: () => '→' },
              {
                title: '新文件名',
                dataIndex: 'new_filename',
                width: '34%',
                render: (text, row) => {
                  if (renameResult) {
                    return <span style={{ fontWeight: 500 }}>{text}</span>;
                  }
                  const draft = renameEditing[row.doc_id];
                  const dirty = draft !== undefined;
                  return (
                    <div className="pm-rename-name">
                      <Input
                        size="small"
                        value={dirty ? draft : text}
                        status={row.status === 'conflict' || row.status === 'invalid' ? 'error' : undefined}
                        onChange={(e) => setRenameEditing((m) => ({ ...m, [row.doc_id]: e.target.value }))}
                        onPressEnter={(e) => e.target.blur()}
                        onBlur={(e) => {
                          if (!dirty) return;
                          const value = e.target.value;
                          if (value === text) {
                            setRenameEditing((m) => {
                              const copy = { ...m };
                              delete copy[row.doc_id];
                              return copy;
                            });
                            return;
                          }
                          commitRenameOverride(row.doc_id, value);
                        }}
                      />
                      {renameOverrides[row.doc_id] !== undefined && (
                        <Tooltip title="恢复为按规则计算的名字">
                          <Button
                            size="small"
                            type="text"
                            icon={<ReloadOutlined />}
                            onClick={() => commitRenameOverride(row.doc_id, null)}
                          />
                        </Tooltip>
                      )}
                    </div>
                  );
                }
              },
              {
                title: '状态',
                dataIndex: 'status',
                width: 96,
                render: (status) => {
                  const meta = RENAME_STATUS[status] || { label: status, tone: 'neutral' };
                  return <Tag style={statusTagStyle(meta.tone)}>{meta.label}</Tag>;
                }
              },
              {
                title: '说明',
                dataIndex: 'reason',
                ellipsis: true,
                render: (reason, row) => reason || row.error || '—'
              }
            ]}
          />
        )}
      </Modal>

      {/* Markdown 预览设置：快捷键与搜索/替换栏默认可见性 */}
      <Modal
        title="Markdown 预览设置"
        open={prefsOpen}
        onCancel={() => setPrefsOpen(false)}
        width={520}
        footer={[
          <Button key="reset" onClick={() => setMdPrefs({ ...MD_PREF_DEFAULTS })}>
            全部恢复默认
          </Button>,
          <Button key="ok" type="primary" onClick={() => setPrefsOpen(false)}>
            关闭
          </Button>
        ]}
      >
        <div style={{ fontSize: 12, color: 'var(--pm-text-3)', marginBottom: 12 }}>
          点击输入框后直接按下组合键即可修改，自动保存到本机。快捷键在预览窗口打开时生效。
        </div>
        <ShortcutRecorder
          label="保存"
          value={mdPrefs.saveShortcut}
          defaultValue={MD_PREF_DEFAULTS.saveShortcut}
          onChange={(v) => setMdPrefs((p) => ({ ...p, saveShortcut: v }))}
        />
        <ShortcutRecorder
          label="打开搜索栏"
          value={mdPrefs.findShortcut}
          defaultValue={MD_PREF_DEFAULTS.findShortcut}
          onChange={(v) => setMdPrefs((p) => ({ ...p, findShortcut: v }))}
        />
        <ShortcutRecorder
          label="打开替换栏"
          value={mdPrefs.replaceShortcut}
          defaultValue={MD_PREF_DEFAULTS.replaceShortcut}
          onChange={(v) => setMdPrefs((p) => ({ ...p, replaceShortcut: v }))}
        />
        <div style={{ height: 1, background: 'var(--pm-border-light)', margin: '12px 0' }} />
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 10 }}>
          <Switch
            size="small"
            checked={mdPrefs.findBarOpen}
            onChange={(v) => setMdPrefs((p) => ({ ...p, findBarOpen: v }))}
          />
          <span style={{ fontSize: 13 }}>打开预览时显示搜索栏</span>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <Switch
            size="small"
            checked={mdPrefs.replaceBarOpen}
            onChange={(v) => setMdPrefs((p) => ({ ...p, replaceBarOpen: v }))}
          />
          <span style={{ fontSize: 13 }}>打开预览时显示替换栏</span>
        </div>
        <div style={{ marginTop: 12, fontSize: 12, color: 'var(--pm-text-3)' }}>
          「显示替换栏」会同时显示搜索栏；默认两者都隐藏，分别用快捷键呼出。
        </div>
      </Modal>

      {/* Markdown / 原始JSON 预览弹窗（标题栏可拖动，右边缘可调整宽度） */}
      <Modal
        title={
          <div
            className="pm-modal-title"
            onMouseDown={startModalDrag}
            style={{ cursor: 'move', userSelect: 'none' }}
            title={`按住标题可拖动窗口 · ${previewTitle}`}
          >
            预览：{previewTitle}
          </div>
        }
        open={previewVisible}
        onCancel={() => setPreviewVisible(false)}
        maskClosable={false}
        width={Math.min(modalWidth, viewportWidth - 24)}
        style={{
          top: modalTop,
          maxWidth: 'calc(100vw - 16px)',
          transform: `translate(${modalOffset.x}px, ${modalOffset.y}px)`,
          transition: 'none'
        }}
        styles={{
          body: {
            paddingTop: 4,
            maxHeight: modalHeight ? `${modalHeight}px` : 'calc(100vh - 180px)',
            overflowY: 'auto'
          }
        }}
        footer={[
          <Button key="close" onClick={() => setPreviewVisible(false)}>
            关闭
          </Button>
        ]}
      >
        {previewLoading ? (
          <div style={{ textAlign: 'center', padding: '40px 0', color: 'var(--pm-text-3)' }}>
            正在加载内容...
          </div>
        ) : (
          <Tabs
            onChange={(key) => {
              if (key === 'json' && previewJsonPaths.length > 0 && !jsonContent) {
                loadPreviewJson(jsonIndex);
              }
            }}
            items={[
              {
                key: 'markdown',
                label: 'Markdown',
                children: (
                  <div style={{ position: 'relative' }}>
                    {/* 右边缘拖拽手柄：调整预览窗口宽度 */}
                    <div
                      onMouseDown={startModalResize}
                      title="拖动调整窗口宽度"
                      style={{
                        position: 'absolute',
                        right: -20,
                        top: 0,
                        bottom: 0,
                        width: 10,
                        cursor: 'ew-resize',
                        zIndex: 10
                      }}
                    />
                    {/* 底边缘 / 右下角拖拽手柄：调整预览窗口高度 */}
                    <div
                      onMouseDown={startModalHeightResize}
                      title="拖动调整窗口高度"
                      style={{
                        position: 'absolute',
                        left: 0,
                        right: 0,
                        bottom: -18,
                        height: 10,
                        cursor: 'ns-resize',
                        zIndex: 10
                      }}
                    />
                    <div
                      onMouseDown={startModalHeightResize}
                      title="拖动调整窗口大小"
                      style={{
                        position: 'absolute',
                        right: -20,
                        bottom: -18,
                        width: 18,
                        height: 18,
                        cursor: 'nwse-resize',
                        zIndex: 11
                      }}
                    />

                    {/* 预览页内直接打分与选标签 */}
                    <div
                      style={{
                        display: 'flex',
                        alignItems: 'center',
                        gap: 8,
                        flexWrap: 'wrap',
                        marginBottom: 8,
                        padding: '4px 8px',
                        background: 'var(--pm-bg-subtle)',
                        border: '1px solid var(--pm-border-light)',
                        borderRadius: 6
                      }}
                    >
                      <span style={{ fontSize: 12, color: 'var(--pm-text-2)' }}>评分</span>
                      <Select
                        size="small"
                        style={{ width: 96 }}
                        value={previewMeta.rating || 0}
                        loading={previewMetaSaving}
                        onChange={handlePreviewRating}
                        options={[
                          { value: 0, label: '未评分' },
                          ...Array.from({ length: 10 }, (_, i) => ({ value: i + 1, label: `${i + 1} 分` }))
                        ]}
                      />
                      <span style={{ fontSize: 12, color: 'var(--pm-text-2)', marginLeft: 8 }}>标签</span>
                      <Select
                        mode="tags"
                        size="small"
                        style={{ minWidth: 240, flex: '0 1 360px' }}
                        value={(previewMeta.tags || []).map((t) => t.name)}
                        loading={previewMetaSaving}
                        onChange={handlePreviewTagsChange}
                        placeholder="选择或输入标签，回车保存"
                        tokenSeparators={[',', '，']}
                        options={tags.map((tag) => ({
                          value: tag.name,
                          label: (
                            <span>
                              <span
                                style={{
                                  display: 'inline-block',
                                  width: 8,
                                  height: 8,
                                  borderRadius: '50%',
                                  background: tag.color,
                                  marginRight: 8
                                }}
                              />
                              {tag.name}
                            </span>
                          )
                        }))}
                      />
                    </div>

                    {/* 工具栏：目录折叠、编辑/预览/实时预览、保存、字体、字号、复制 */}
                    <div
                      style={{
                        display: 'flex',
                        alignItems: 'center',
                        gap: 4,
                        flexWrap: 'wrap',
                        marginBottom: 8
                      }}
                    >
                      <Tooltip title={tocCollapsed ? '展开目录' : '折叠目录'}>
                        <Button
                          size="small"
                          type="text"
                          icon={tocCollapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />}
                          onClick={() => setTocCollapsed((v) => !v)}
                          disabled={!tocHeadings.length}
                        />
                      </Tooltip>
                      <Tooltip title="编辑">
                        <Button
                          size="small"
                          type={editorMode === 'edit' ? 'primary' : 'text'}
                          icon={<EditOutlined />}
                          onClick={() => setEditorMode('edit')}
                        />
                      </Tooltip>
                      <Tooltip title="预览">
                        <Button
                          size="small"
                          type={editorMode === 'preview' ? 'primary' : 'text'}
                          icon={<EyeOutlined />}
                          onClick={() => setEditorMode('preview')}
                        />
                      </Tooltip>
                      <Tooltip title="实时预览（光标所在行显示 Markdown 源码，其余行渲染）">
                        <Button
                          size="small"
                          type={editorMode === 'live' ? 'primary' : 'text'}
                          icon={<ThunderboltOutlined />}
                          onClick={() => setEditorMode('live')}
                        />
                      </Tooltip>
                      <Tooltip title={previewHasPdf ? (pdfPaneVisible ? '关闭右侧PDF对比' : '在右侧显示原始PDF对比') : '该文档没有原始PDF文件'}>
                        <Button
                          size="small"
                          type={pdfPaneVisible ? 'primary' : 'text'}
                          icon={<FilePdfOutlined />}
                          disabled={!previewHasPdf}
                          onClick={() => setPdfPaneVisible((v) => !v)}
                        />
                      </Tooltip>
                      <Tooltip title="保存">
                        <Button
                          size="small"
                          type="text"
                          icon={<SaveOutlined />}
                          loading={savingMd}
                          disabled={!mdDirty || !editContent}
                          onClick={handleSaveMarkdown}
                        />
                      </Tooltip>
                      {mdDirty && <Tag color="orange">未保存</Tag>}
                      <Tooltip
                        title={previewRebuildDisabledReason
                          || '用本地 OCR JSON 重新生成 Markdown（含表格），不调用 MinerU；旧文件自动备份'}
                      >
                        <span>
                          <Button
                            size="small"
                            type="text"
                            icon={<SyncOutlined />}
                            loading={previewRebuilding}
                            disabled={!!previewRebuildDisabledReason}
                            onClick={handlePreviewRebuild}
                          >
                            重建
                          </Button>
                        </span>
                      </Tooltip>
                      <Tooltip title="导出为 EPUB（表格转 XHTML、图片内嵌，保存到设置中的导出目录）">
                        <span>
                          <Button
                            size="small"
                            type="text"
                            icon={<BookOutlined />}
                            loading={exportingEpub}
                            disabled={!previewContent}
                            onClick={handleExportEpub}
                          >
                            EPUB
                          </Button>
                        </span>
                      </Tooltip>
                      <Tooltip title="导出为 Word（.docx，保留标题/列表/表格/代码块/引用，图片真正内嵌，保存到设置中的导出目录）">
                        <span>
                          <Button
                            size="small"
                            type="text"
                            icon={<FileWordOutlined />}
                            loading={exportingDocx}
                            disabled={!previewContent}
                            onClick={handleExportDocx}
                          >
                            Word
                          </Button>
                        </span>
                      </Tooltip>
                      <Dropdown
                        menu={{
                          items: [
                            { key: 't2s', label: '繁體 → 简体' },
                            { key: 's2t', label: '简体 → 繁體' }
                          ],
                          onClick: ({ key }) => handleConvertZh(key)
                        }}
                      >
                        <Tooltip title="中文繁简转换（词库来自 zhconv；繁→简可配置自定义映射，转换结果进入编辑区，检查后保存）">
                          <span>
                            <Button
                              size="small"
                              type="text"
                              icon={<SwapOutlined />}
                              loading={convertingZh}
                              disabled={!previewContent}
                            >
                              繁简
                            </Button>
                          </span>
                        </Tooltip>
                      </Dropdown>
                      <span style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 8 }}>
                        <span style={{ fontSize: 12, color: 'var(--pm-text-2)' }}>字体</span>
                        <Select
                          size="small"
                          showSearch
                          optionFilterProp="label"
                          value={mdFontKey}
                          onChange={setMdFontKey}
                          style={{ width: 170 }}
                          options={fontOptions}
                          placeholder="选择字体"
                        />
                        <Tooltip title="加载本地字体文件">
                          <Button
                            size="small"
                            type="text"
                            icon={<FontSizeOutlined />}
                            loading={fontLoading}
                            onClick={() => {
                              setFontPathInput(customFontPath);
                              setFontModalVisible(true);
                            }}
                          />
                        </Tooltip>
                        <span style={{ fontSize: 12, color: 'var(--pm-text-2)' }}>字号</span>
                        <InputNumber
                          size="small"
                          min={10}
                          max={32}
                          value={mdFontSize}
                          onChange={(v) => setMdFontSize(Number(v) || 14)}
                          style={{ width: 70 }}
                        />
                        <Tooltip title="搜索（打开搜索栏，快捷键可在预览设置中修改）">
                          <Button
                            size="small"
                            type="text"
                            icon={<SearchOutlined />}
                            onClick={() => {
                              setSearchVisible(true);
                              setTimeout(() => {
                                searchInputRef.current?.focus();
                                searchInputRef.current?.select?.();
                              }, 0);
                            }}
                          />
                        </Tooltip>
                        <Tooltip title="预览设置：快捷键、搜索/替换栏默认显示">
                          <Button
                            size="small"
                            type="text"
                            icon={<SettingOutlined />}
                            onClick={() => setPrefsOpen(true)}
                          />
                        </Tooltip>
                        <Tooltip title="复制 Markdown 内容">
                          <Button
                            size="small"
                            type="text"
                            icon={<CopyOutlined />}
                            onClick={handleCopyMarkdownContent}
                            disabled={!previewContent}
                          />
                        </Tooltip>
                      </span>
                    </div>

                    {/* 重建结果：成功/跳过/失败都留在预览窗口内，避免静默 */}
                    {previewRebuildNotice && (
                      <Alert
                        style={{ marginBottom: 8 }}
                        type={previewRebuildNotice.type}
                        showIcon
                        closable
                        onClose={() => setPreviewRebuildNotice(null)}
                        message={previewRebuildNotice.text}
                      />
                    )}

                    {/* 搜索与替换：默认隐藏，Ctrl+F / Ctrl+H 或工具栏按钮展开；快捷键与默认可见性可在预览设置里改 */}
                    {searchVisible && (
                    <div style={{ marginBottom: 8 }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                        <Input
                          size="small"
                          allowClear
                          ref={searchInputRef}
                          prefix={<SearchOutlined />}
                          placeholder={`搜索关键词（${mdPrefs.findShortcut}）`}
                          value={searchTerm}
                          onChange={(e) => setSearchTerm(e.target.value)}
                          onPressEnter={() => gotoSearchMatch(1)}
                          status={findError ? 'error' : undefined}
                          style={{ width: 220 }}
                        />
                        <Checkbox
                          checked={searchUseRegex}
                          onChange={(e) => setSearchUseRegex(e.target.checked)}
                        >
                          正则
                        </Checkbox>
                        <Checkbox
                          checked={searchCaseSensitive}
                          onChange={(e) => setSearchCaseSensitive(e.target.checked)}
                        >
                          大小写
                        </Checkbox>
                        <Checkbox
                          checked={searchWholeWord}
                          onChange={(e) => setSearchWholeWord(e.target.checked)}
                        >
                          整词
                        </Checkbox>
                        <Button size="small" onClick={() => gotoSearchMatch(-1)} disabled={!searchCount}>
                          上一个
                        </Button>
                        <Button size="small" onClick={() => gotoSearchMatch(1)} disabled={!searchCount}>
                          下一个
                        </Button>
                        <span style={{ fontSize: 12, color: 'var(--pm-text-3)', minWidth: 48 }}>
                          {searchCount ? `${searchIndex + 1}/${searchCount}` : '0/0'}
                        </span>
                        <Button
                          size="small"
                          type={replaceExpanded ? 'default' : 'text'}
                          onClick={() => setReplaceExpanded((v) => !v)}
                        >
                          {replaceExpanded ? '收起替换' : `替换（${mdPrefs.replaceShortcut}）`}
                        </Button>
                      </div>
                      {replaceExpanded && (
                        <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap', marginTop: 8 }}>
                          <Input
                            size="small"
                            allowClear
                            ref={replaceInputRef}
                            placeholder="替换为，支持 $1 / $<name> / $& / $$"
                            value={replaceTerm}
                            onChange={(e) => setReplaceTerm(e.target.value)}
                            onPressEnter={() => handleReplace(true)}
                            style={{ width: 240 }}
                          />
                          <Button
                            size="small"
                            onClick={() => handleReplace(true)}
                            disabled={!searchTerm || !searchCount || !!findError}
                          >
                            替换
                          </Button>
                          <Button
                            size="small"
                            danger
                            onClick={() => handleReplace(false)}
                            disabled={!searchTerm || !!findError}
                          >
                            全部替换
                          </Button>
                          <Tooltip
                            title={
                              <span>
                                替换串按 JavaScript 规则展开：$1~$99 为编号捕获组，$&lt;name&gt; 为命名捕获组，
                                $&amp; 为整体匹配，$$ 表示字面 $；不存在的组号保持原样。
                                <br />
                                正则写 ^(\d+)、替换写 # $1 即把数字前加 “# ”。
                              </span>
                            }
                          >
                            <QuestionCircleOutlined style={{ color: 'var(--pm-text-3)', cursor: 'help' }} />
                          </Tooltip>
                        </div>
                      )}
                      {findError && (
                        <div style={{ marginTop: 6, fontSize: 12, color: 'var(--pm-error)' }}>
                          正则表达式错误：{findError}
                        </div>
                      )}
                      {previewSourceHint && (
                        <div style={{ marginTop: 6, fontSize: 12, color: 'var(--pm-text-3)' }}>
                          预览模式只匹配渲染后的正文，行首「#」等 Markdown 标记与「^」「$」行锚点请切换到「编辑」或「实时预览」模式。
                        </div>
                      )}
                      <div style={{ marginTop: 6 }}>
                        <Button size="small" type="text" onClick={() => setSearchVisible(false)}>
                          关闭搜索栏
                        </Button>
                      </div>
                    </div>
                    )}

                    {/* 目录 + 内容：编辑/预览/实时预览三种模式下都显示目录 */}
                    <div style={{ display: 'flex', gap: 12, alignItems: 'flex-start', flexDirection: stackedMdPanes ? 'column' : 'row' }}>
                      {!tocCollapsed && tocHeadings.length > 0 && (
                        <div
                          style={{
                            flex: stackedMdPanes ? '0 0 auto' : `0 0 ${tocWidth}px`,
                            width: stackedMdPanes ? '100%' : tocWidth,
                            maxHeight: stackedMdPanes ? 140 : mdPaneHeight,
                            overflow: 'auto',
                            background: 'var(--pm-bg-subtle)',
                            border: '1px solid var(--pm-border-light)',
                            borderRadius: 6,
                            padding: '8px 6px'
                          }}
                        >
                          <div style={{ padding: '2px 8px 6px', fontSize: 12, color: 'var(--pm-text-3)' }}>
                            目录
                          </div>
                          {tocHeadings.map((h) => (
                            <div
                              key={h.id}
                              className="md-toc-item"
                              title={h.text}
                              style={{ paddingLeft: 8 + (h.level - 1) * 10 }}
                              onClick={() => handleTocClick(h)}
                            >
                              {h.text}
                            </div>
                          ))}
                        </div>
                      )}
                      {/* 目录右边缘拖拽：调整目录宽度 */}
                      {!stackedMdPanes && !tocCollapsed && tocHeadings.length > 0 && (
                        <div
                          className="pm-pane-splitter"
                          onMouseDown={startTocResize}
                          title="拖动调整目录宽度"
                          style={{ height: mdPaneHeight }}
                        />
                      )}
                      {/* 正文区：宽度可拖拽（bodyWidth=0 时铺满） */}
                      <div
                        style={{
                          flex: '1 1 0',
                          minWidth: 0,
                          maxWidth: bodyWidth || undefined,
                          display: 'flex',
                          alignItems: 'flex-start'
                        }}
                      >
                      {editorMode === 'edit' && (
                        <Input.TextArea
                          ref={editorRef}
                          value={editContent}
                          onChange={(e) => setEditContent(e.target.value)}
                          spellCheck={false}
                          style={{
                            flex: 1,
                            minWidth: 0,
                            height: mdPaneHeight,
                            resize: 'none',
                            fontFamily: mdFontCss,
                            fontSize: `${mdFontSize}px`,
                            lineHeight: 1.7,
                            background: 'var(--pm-bg-subtle)'
                          }}
                        />
                      )}
                      {editorMode === 'live' && (
                        <div
                          ref={liveEditorHostRef}
                          className="live-editor"
                          style={{
                            flex: 1,
                            minWidth: 0,
                            height: mdPaneHeight,
                            overflow: 'hidden',
                            background: 'var(--pm-bg-subtle)',
                            border: '1px solid var(--pm-border-light)',
                            borderRadius: 6
                          }}
                        />
                      )}
                      {editorMode === 'preview' && (
                        <div
                          ref={previewContentRef}
                          className="md-preview"
                          dangerouslySetInnerHTML={{ __html: previewParsed.html }}
                          style={{
                            flex: 1,
                            minWidth: 0,
                            maxHeight: mdPaneHeight,
                            overflow: 'auto',
                            background: 'var(--pm-bg-subtle)',
                            border: '1px solid var(--pm-border-light)',
                            borderRadius: 6,
                            padding: 16,
                            fontFamily: mdFontCss,
                            fontSize: `${mdFontSize}px`,
                            lineHeight: 1.8,
                            wordBreak: 'break-word'
                          }}
                        />
                      )}
                      </div>
                      {/* 正文右边缘拖拽：调整正文宽度 */}
                      {!stackedMdPanes && (
                        <div
                          className="pm-pane-splitter"
                          onMouseDown={startBodyResize}
                          title="拖动调整正文宽度"
                          style={{ height: mdPaneHeight }}
                        />
                      )}
                      {/* 原始 PDF 对比面板：显示在 Markdown 内容右侧，与内容区同高 */}
                      {pdfPaneVisible && previewHasPdf && (
                        <div
                          style={{
                            flex: stackedMdPanes ? '0 0 auto' : '0 1 42%',
                            width: stackedMdPanes ? '100%' : undefined,
                            minWidth: stackedMdPanes ? 0 : 260,
                            height: stackedMdPanes ? 320 : mdPaneHeight,
                            border: '1px solid var(--pm-border-light)',
                            borderRadius: 6,
                            overflow: 'hidden',
                            background: 'var(--pm-bg-container)'
                          }}
                        >
                          <iframe
                            key={previewDocId}
                            src={`/api/documents/${previewDocId}/pdf#page=1&view=FitH`}
                            title="原始PDF对比"
                            style={{ width: '100%', height: '100%', border: 'none', display: 'block' }}
                          />
                        </div>
                      )}
                    </div>
                  </div>
                )
              },
              {
                key: 'json',
                label: `原始 JSON（${previewJsonPaths.length}）`,
                children: previewJsonPaths.length === 0 ? (
                  <div style={{ padding: '24px 0', color: 'var(--pm-text-3)', textAlign: 'center' }}>
                    该文档暂无 OCR 原始 JSON 结果
                  </div>
                ) : (
                  <div>
                    <Select
                      value={jsonIndex}
                      onChange={loadPreviewJson}
                      style={{ width: '100%', marginBottom: 12 }}
                      options={previewJsonPaths.map((p, i) => ({
                        value: i,
                        label: `${i + 1}. ${p.split(/[\\/]/).pop()}`
                      }))}
                    />
                    {jsonLoading ? (
                      <div style={{ textAlign: 'center', padding: '40px 0', color: 'var(--pm-text-3)' }}>
                        正在加载 JSON 内容...
                      </div>
                    ) : (
                      <pre
                        style={{
                          maxHeight: '55vh',
                          overflow: 'auto',
                          background: 'var(--pm-bg-subtle)',
                          border: '1px solid var(--pm-border-light)',
                          borderRadius: 6,
                          padding: 12,
                          whiteSpace: 'pre',
                          fontSize: '12px',
                          lineHeight: 1.6,
                          fontFamily: 'Consolas, Monaco, "Courier New", monospace',
                          margin: 0
                        }}
                      >
                        {jsonContent || '点击上方文件加载内容（切换文件自动加载）'}
                      </pre>
                    )}
                  </div>
                )
              }
            ]}
          />
        )}
      </Modal>

      {/* 加载本地字体文件 */}
      <Modal
        title="加载本地字体文件"
        open={fontModalVisible}
        onCancel={() => setFontModalVisible(false)}
        onOk={handleLoadFontFile}
        confirmLoading={fontLoading}
        okText="按路径加载"
        cancelText="取消"
        destroyOnHidden
      >
        <div style={{ marginBottom: 12, color: 'var(--pm-text-2)', fontSize: 13 }}>
          支持 .ttf / .otf / .woff / .woff2 字体文件。推荐使用下方「选择字体文件」直接选择，无需填写路径。
        </div>
        <div style={{ marginBottom: 12 }}>
          <Upload
            accept=".ttf,.otf,.woff,.woff2,.ttc"
            showUploadList={false}
            beforeUpload={(file) => {
              applyCustomFont(file, file.name).then((ok) => {
                if (ok) {
                  loadedCustomFontRef.current = file.name;
                  setFontModalVisible(false);
                }
              });
              return false;
            }}
          >
            <Button type="primary" icon={<UploadOutlined />} loading={fontLoading}>
              选择字体文件
            </Button>
          </Upload>
        </div>
        <div style={{ marginBottom: 6, color: 'var(--pm-text-3)', fontSize: 12 }}>或填写本机绝对路径：</div>
        <Input
          value={fontPathInput}
          onChange={(e) => setFontPathInput(e.target.value)}
          onPressEnter={handleLoadFontFile}
          placeholder={'例如 C:\\Windows\\Fonts\\simsun.ttc'}
          allowClear
        />
        {recentFonts.length > 0 && (
          <div style={{ marginTop: 12 }}>
            <div style={{ fontSize: 12, color: 'var(--pm-text-3)', marginBottom: 6 }}>最近使用：</div>
            <Space wrap size={[8, 8]}>
              {recentFonts.map((f, idx) => (
                <Tag
                  key={f.path + idx}
                  color="blue"
                  style={{ cursor: 'pointer' }}
                  onClick={() => applyCustomFont(f.path, f.label).then((ok) => {
                    if (ok) {
                      loadedCustomFontRef.current = f.path;
                      setFontModalVisible(false);
                    }
                  })}
                >
                  {f.label}
                </Tag>
              ))}
            </Space>
          </div>
        )}
        <div style={{ marginTop: 12, fontSize: 12, color: 'var(--pm-text-3)', lineHeight: 1.7 }}>
          提示：.ttc 是字体集合（一个文件含多种字体），浏览器只会加载其中第一个字面，效果可能与预期不同，建议改用 .ttf/.otf。
        </div>
        {customFontPath && (
          <div style={{ marginTop: 8, fontSize: 12, color: 'var(--pm-text-3)', wordBreak: 'break-all' }}>
            当前已加载：{customFontPath}
          </div>
        )}
      </Modal>

      {/* 系统设置弹窗：Token 仅回显掩码，目录改动只影响后续新产物 */}
      <Modal
        title="系统设置"
        open={settingsVisible}
        onCancel={() => setSettingsVisible(false)}
        width={Math.min(760, viewportWidth - 32)}
        style={{ top: viewportHeight < 900 ? 16 : 40 }}
        styles={{ body: { maxHeight: `calc(100vh - ${viewportHeight < 900 ? 168 : 196}px)`, overflowY: 'auto', paddingRight: 8 } }}
        footer={[
          <Button key="restore" onClick={handleRestoreDefaults} disabled={settingsLoading || settingsSaving}>
            恢复默认值
          </Button>,
          <Button key="cancel" onClick={() => setSettingsVisible(false)}>
            取消
          </Button>,
          <Button
            key="save"
            type="primary"
            icon={<SaveOutlined />}
            onClick={handleSaveSettings}
            loading={settingsSaving}
            disabled={settingsLoading}
          >
            保存
          </Button>
        ]}
      >
        <Tabs
          activeKey={settingsTab}
          onChange={setSettingsTab}
          items={[
            {
              key: 'basic',
              label: '凭据与目录',
              children: (
                <div>
                  <div
                    style={{
                      background: 'var(--pm-warning-bg)',
                      border: '1px solid var(--pm-warning-border)',
                      borderRadius: 6,
                      padding: '8px 12px',
                      marginBottom: 16,
                      color: 'var(--pm-warning-text)',
                      fontSize: 13
                    }}
                  >
                    目录修改只影响之后新生成的 Markdown / JSON / 图片，不会自动迁移历史产物。
                    旧文档可能因图片不在新图片目录而显示缺图，旧 JSON / Markdown 也可能无法预览；如需保留请手动迁移文件或维持原目录。
                    EPUB 与 Word 导出目录只决定之后导出电子书的落盘位置，可随时修改。
                  </div>

                  <div style={{ marginBottom: 16 }}>
                    <div style={{ fontWeight: 500, marginBottom: 6 }}>MinerU API 基址</div>
                    <Input
                      value={settingsForm.mineru_api_url || ''}
                      onChange={(e) => updateSettingsField('mineru_api_url', e.target.value)}
                      placeholder="https://mineru.net/api/v4"
                    />
                    <div style={{ color: 'var(--pm-text-3)', fontSize: 12, marginTop: 4 }}>
                      默认：{settingsData?.defaults?.mineru_api_url}；填错会导致 OCR 请求全部失败，修改后仅对之后发起的请求生效。
                    </div>
                  </div>

                  <div style={{ marginBottom: 16 }}>
                    <div style={{ fontWeight: 500, marginBottom: 6 }}>MinerU Token</div>
                    <Input.Password
                      value={settingsTokenInput}
                      onChange={(e) => setSettingsTokenInput(e.target.value)}
                      placeholder={
                        settingsData?.has_mineru_token
                          ? `当前已配置：${settingsData.mineru_token_masked}（留空则不修改）`
                          : '请输入 MinerU Token'
                      }
                      autoComplete="new-password"
                    />
                    <div style={{ color: 'var(--pm-text-3)', fontSize: 12, marginTop: 4 }}>
                      接口只返回掩码，日志不记录明文；留空表示保持当前 Token 不变。
                    </div>
                  </div>

                  {SETTINGS_FOLDER_FIELDS.map((field) => {
                    const stat = settingsData?.[field.statKey];
                    return (
                      <div key={field.target} style={{ marginBottom: 16 }}>
                        <div style={{ fontWeight: 500, marginBottom: 6 }}>{field.label}</div>
                        <Space.Compact style={{ width: '100%' }}>
                          <Input
                            value={settingsForm[field.key] || ''}
                            onChange={(e) => updateSettingsField(field.key, e.target.value)}
                            placeholder="绝对路径，如 E:\pdf_manager_output\images"
                          />
                          <Button
                            icon={<FolderOpenOutlined />}
                            onClick={() => openSettingsPicker(field.target)}
                          >
                            浏览
                          </Button>
                        </Space.Compact>
                        <div style={{ color: 'var(--pm-text-3)', fontSize: 12, marginTop: 4 }}>
                          当前占用：{stat ? `${stat.file_count} 个文件 / ${formatBytes(stat.total_bytes)}` : '—'}
                          {settingsData?.defaults?.[field.key] && (
                            <>
                              {' '}　默认：<code>{settingsData.defaults[field.key]}</code>
                            </>
                          )}
                        </div>
                      </div>
                    );
                  })}

                  <div style={{ marginBottom: 4 }}>
                    <div style={{ fontWeight: 500, marginBottom: 6 }}>繁→简自定义映射</div>
                    <Input.TextArea
                      rows={6}
                      value={settingsForm.zh_t2s_mapping || ''}
                      onChange={(e) => updateSettingsField('zh_t2s_mapping', e.target.value)}
                      placeholder={'每行一条：繁体词=>简体词\n例如：乾隆=>乾隆\n# 开头的行视为注释'}
                      style={{ fontFamily: 'monospace' }}
                    />
                    <div style={{ color: 'var(--pm-text-3)', fontSize: 12, marginTop: 4 }}>
                      仅对「繁→简」转换生效：命中的词按映射结果输出，优先于内置词库（可用于保护「乾隆」不被误转为「干隆」等场景）。留空表示不使用自定义映射。
                    </div>
                  </div>
                </div>
              )
            },
            ...SETTINGS_FIELD_GROUPS.map((group) => ({
              key: group.key,
              label: group.label,
              children: (
                <div style={{ paddingTop: 8 }}>
                  {group.fields.map((field) => renderSettingsField(field))}
                </div>
              )
            }))
          ]}
        />

        {settingsPickerTarget && (
          <div
            style={{
              border: '1px solid var(--pm-border)',
              borderRadius: 6,
              padding: 12,
              marginTop: 4
            }}
          >
            <Space style={{ marginBottom: 8, width: '100%', justifyContent: 'space-between' }}>
              <Space>
                <Button
                  size="small"
                  icon={<ArrowUpOutlined />}
                  disabled={pickerParent === null}
                  onClick={() => loadPickerFolders(pickerParent ?? '')}
                >
                  上级
                </Button>
                <Button size="small" icon={<HomeOutlined />} onClick={() => loadPickerFolders('')}>
                  盘符
                </Button>
              </Space>
              <span style={{ fontWeight: 600, fontSize: 13 }}>
                {pickerPath || '此电脑（请选择磁盘）'}
              </span>
            </Space>
            <List
              size="small"
              bordered
              loading={pickerLoading}
              dataSource={pickerList}
              style={{ maxHeight: 220, overflowY: 'auto', marginBottom: 8 }}
              locale={{ emptyText: pickerPath ? '该目录下没有子文件夹' : '未检测到可用磁盘' }}
              renderItem={(folder) => (
                <List.Item
                  key={folder.path}
                  style={{ cursor: 'pointer', padding: '6px 12px' }}
                  onClick={() => loadPickerFolders(folder.path)}
                >
                  <span>
                    <FolderOutlined style={{ color: 'var(--pm-warning)', marginRight: 8 }} />
                    {folder.name}
                  </span>
                </List.Item>
              )}
            />
            <Space>
              <Button size="small" type="primary" onClick={handlePickerChoose} disabled={!pickerPath}>
                选择当前目录
              </Button>
              <Button size="small" onClick={() => setSettingsPickerTarget(null)}>
                收起
              </Button>
            </Space>
          </div>
        )}
      </Modal>

      {/* 重命名弹窗 */}
      <Modal
        title="重命名文档"
        open={renameVisible}
        onCancel={() => setRenameVisible(false)}
        onOk={handleRename}
        confirmLoading={renameLoading}
        okText="保存"
        cancelText="取消"
        width={Math.min(520, viewportWidth - 32)}
        destroyOnHidden
      >
        <div style={{ marginBottom: 8, color: 'var(--pm-text-2)', fontSize: '13px' }}>
          原文件名：{renameDoc?.filename}
        </div>
        <Input
          value={renameValue}
          onChange={(e) => setRenameValue(e.target.value)}
          onPressEnter={handleRename}
          placeholder="输入新的文件名（无需填写 .pdf 扩展名）"
          autoFocus
          addonAfter=".pdf"
        />
        <div style={{ marginTop: 8, color: 'var(--pm-text-3)', fontSize: '12px' }}>
          将同步重命名磁盘上的 PDF 文件{renameDoc?.markdown_path ? '和已生成的 Markdown 文件' : ''}；文件名不能包含 \ / : * ? " &lt; &gt; | 字符。
        </div>
      </Modal>

      <Modal
        title="扫描目录"
        open={isScanModalVisible}
        onCancel={() => setIsScanModalVisible(false)}
        width={Math.min(640, viewportWidth - 32)}
        footer={[
          <Button key="back" onClick={() => setIsScanModalVisible(false)}>
            取消
          </Button>,
          <Button
            key="submit"
            type="primary"
            icon={<ScanOutlined />}
            onClick={() => handleScanDirectory()}
            loading={scanning}
            disabled={!browsePath}
          >
            扫描当前目录
          </Button>
        ]}
      >
        {/* 目录导航工具栏 */}
        <Space style={{ marginBottom: 12, width: '100%', justifyContent: 'space-between' }}>
          <Space>
            <Button
              size="small"
              icon={<ArrowUpOutlined />}
              disabled={folderParent === null}
              onClick={() => fetchFolders(folderParent ?? '')}
            >
              上级
            </Button>
            <Button size="small" icon={<HomeOutlined />} onClick={() => fetchFolders('')}>
              盘符
            </Button>
          </Space>
          <span style={{ fontWeight: 600, fontSize: '13px' }}>
            {browsePath || '此电脑（请选择磁盘）'}
          </span>
        </Space>

        {/* 文件夹列表：单击进入下级目录 */}
        <List
          size="small"
          bordered
          loading={foldersLoading}
          dataSource={folderList}
          style={{ maxHeight: 300, overflowY: 'auto', marginBottom: 12 }}
          locale={{ emptyText: browsePath ? '该目录下没有子文件夹' : '未检测到可用磁盘' }}
          renderItem={folder => (
            <List.Item
              key={folder.path}
              style={{ cursor: 'pointer', padding: '6px 12px' }}
              onClick={() => fetchFolders(folder.path)}
            >
              <span>
                <FolderOutlined style={{ color: 'var(--pm-warning)', marginRight: 8 }} />
                {folder.name}
              </span>
            </List.Item>
          )}
        />

        {/* 手动输入路径跳转（兼容直接粘贴路径） */}
        <Input.Search
          placeholder="也可直接粘贴目录路径，如 D:\Documents\PDFs，回车跳转"
          value={scanPath}
          onChange={(e) => setScanPath(e.target.value)}
          onSearch={handleJumpToPath}
          enterButton="跳转"
          style={{ marginBottom: 8 }}
        />
        <div style={{ color: 'var(--pm-text-3)', fontSize: '12px' }}>
          点击文件夹进入下级目录；扫描会递归查找所选目录下的所有 PDF 文件，已存在的文件自动跳过。
          {browsePath && (
            <div style={{ marginTop: 4 }}>
              当前选择：<code>{browsePath}</code>
            </div>
          )}
        </div>
      </Modal>

      {/* 全文检索抽屉：索引状态、构建入口、检索结果与分页 */}
      <Drawer
        title="Markdown 全文检索"
        open={searchDrawerVisible}
        onClose={() => setSearchDrawerVisible(false)}
        width={Math.min(720, viewportWidth - 16)}
      >

        {/* 索引状态区：未构建 / 构建中 / 就绪 / 失败 */}
        {!searchStatus && searchStatusLoading && (
          <div style={{ textAlign: 'center', padding: '24px 0' }}>
            <Spin tip="正在读取索引状态…" />
          </div>
        )}
        {searchStatus && (
          <div>
            {searchBuilding ? (
              <Alert
                type="info"
                showIcon
                message={searchStatus.mode === 'full' ? '正在全量重建索引' : '正在增量更新索引'}
                description={(
                  <div>
                    <Progress percent={searchProgressPercent} size="small" status="active" />
                    <div style={{ fontSize: 12, marginTop: 4 }}>
                      已处理 {searchStatus.processed} / {searchStatus.total || '—'} 篇；
                      新增 {searchStatus.added}，更新 {searchStatus.updated}，删除 {searchStatus.deleted}，
                      跳过 {searchStatus.skipped}，失败 {searchStatus.failed}
                    </div>
                    <div style={{ fontSize: 12, marginTop: 2, color: 'var(--pm-text-2)' }}>
                      {searchReady ? '旧索引仍可正常检索。' : '首次构建完成前暂不可检索。'}
                    </div>
                  </div>
                )}
              />
            ) : searchStatus.last_error ? (
              <Alert
                type="error"
                showIcon
                message="索引构建失败 / 索引不可用"
                description={(
                  <div>
                    <div style={{ wordBreak: 'break-all' }}>{searchStatus.last_error}</div>
                    {searchReady ? (
                      <div style={{ fontSize: 12, marginTop: 4 }}>
                        旧索引仍可检索：{searchStatus.doc_count} 篇，约 {formatBytes(searchStatus.index_size)}。
                        可直接重试全量重建。
                      </div>
                    ) : (
                      <div style={{ fontSize: 12, marginTop: 4 }}>
                        可点击「全量重建」重新生成索引；若持续失败请检查索引目录权限与日志。
                      </div>
                    )}
                  </div>
                )}
              />
            ) : searchStatus.needs_rebuild ? (
              <Alert
                type="warning"
                showIcon
                message="索引目录已变更，需要重建索引"
                description={(
                  <div>
                    <div style={{ wordBreak: 'break-all' }}>
                      当前索引目录：{searchStatus.index_path}
                    </div>
                    <div style={{ fontSize: 12, marginTop: 4 }}>
                      该目录中还没有可用索引，重建完成前检索不可用；点击「全量重建」按新目录重新生成。
                    </div>
                  </div>
                )}
              />
            ) : searchReady ? (
              <Alert
                type={searchStatus.doc_count > 0 ? 'success' : 'warning'}
                showIcon
                message={searchStatus.doc_count > 0
                  ? `索引就绪：${searchStatus.doc_count} 篇文档`
                  : '索引为空（已构建，但还没有可检索的文档）'}
                description={(
                  <div>
                    <Space size={[14, 4]} wrap>
                      <span>索引文档：{searchStatus.doc_count}</span>
                      <span>数据库可检索：{searchStatus.source_doc_count}</span>
                      <span>索引大小：{formatBytes(searchStatus.index_size)}</span>
                    </Space>
                    <div style={{ fontSize: 12, color: 'var(--pm-text-3)', marginTop: 4, wordBreak: 'break-all' }}>
                      目录：{searchStatus.index_path}
                      {searchStatus.finished_at ? `　最近构建：${searchStatus.finished_at}` : ''}
                    </div>
                    {searchStatus.failed > 0 && (
                      <div style={{ fontSize: 12, color: 'var(--pm-warning-text)', marginTop: 4 }}>
                        上次构建有 {searchStatus.failed} 篇文档失败，详情见日志。
                      </div>
                    )}
                  </div>
                )}
              />
            ) : (
              <Alert
                type="warning"
                showIcon
                message="索引尚未构建"
                description={(
                  <div>
                    <div>数据库中已有 {searchStatus.source_doc_count} 篇可检索的 Markdown 文档。</div>
                    <div style={{ fontSize: 12, marginTop: 4 }}>
                      点击「全量重建」建立索引；之后可用「增量更新」只处理有变化的文档。
                    </div>
                  </div>
                )}
              />
            )}

            <Space style={{ marginTop: 12 }} wrap>
              <Button
                icon={<SyncOutlined />}
                onClick={() => handleSearchBuild('update')}
                disabled={searchBuildDisabled}
                loading={searchBuildLoading}
              >
                增量更新
              </Button>
              <Button
                type="primary"
                icon={<ReloadOutlined />}
                onClick={() => handleSearchBuild('full')}
                disabled={searchBuildDisabled}
                loading={searchBuildLoading}
              >
                全量重建
              </Button>
              <Button
                size="small"
                type="text"
                icon={<ReloadOutlined />}
                onClick={refreshSearchStatus}
                loading={searchStatusLoading}
              >
                刷新状态
              </Button>
            </Space>

            {searchBuildError && (
              <Alert
                type={searchBuildError.type}
                showIcon
                message={searchBuildError.text}
                style={{ marginTop: 8 }}
              />
            )}
          </div>
        )}

        {/* 检索模式：智能分词 / 原文精确子串 */}
        <div style={{ marginTop: 16, display: 'flex', gap: 10, alignItems: 'center', flexWrap: 'wrap' }}>
          <Segmented
            size="small"
            value={searchMode}
            disabled={!searchReady}
            onChange={(v) => {
              setSearchMode(v);
              if (searchExecuted && searchKeyword.trim()) runSearch(1, undefined, v);
            }}
            options={[
              { label: '智能', value: 'smart' },
              { label: '精确', value: 'exact' }
            ]}
          />
          <span style={{ fontSize: 12, color: 'var(--pm-text-3)' }}>
            {searchMode === 'exact'
              ? '按原文连续子串匹配：忽略 Markdown 标记与空白，不区分大小写'
              : '自动分词后加权匹配；完整匹配不足时会放宽为部分匹配'}
          </span>
        </div>

        {/* 搜索框：回车检索；中文输入法组合期间不触发 */}
        <div style={{ marginTop: 12 }}>
          <Input.Search
            value={searchKeyword}
            onChange={(e) => {
              const value = e.target.value;
              if (value) {
                setSearchKeyword(value);
              } else {
                // 清空输入（含 allowClear）时恢复初始态，避免残留旧结果
                handleSearchClear();
              }
            }}
            onSearch={(_, e) => {
              // 输入法组合中按回车是确认候选词，不应发起检索；
              // 部分浏览器在 keydown 前已触发 compositionend，因此同时检查事件的 isComposing
              if (searchComposingRef.current || e?.nativeEvent?.isComposing) return;
              runSearch(1);
            }}
            onCompositionStart={() => { searchComposingRef.current = true; }}
            onCompositionEnd={() => { searchComposingRef.current = false; }}
            placeholder={searchReady
              ? (searchMode === 'exact' ? '输入要完整匹配的字符串，回车检索' : '输入关键词，回车检索（中文自动分词）')
              : '索引未就绪，请先构建索引'}
            enterButton="检索"
            allowClear
            disabled={!searchReady}
            loading={searching}
          />
          {searchExecuted && !searching && !searchError && (
            <div style={{ marginTop: 6, fontSize: 12, color: 'var(--pm-text-3)' }}>
              共 {searchTotal} 条结果，用时 {searchTookMs} ms
              {searchTruncated && '（仅校验了前 500 条候选，可缩短关键词以获得精确总数）'}
            </div>
          )}
        </div>

        {searchRelaxed && !searching && !searchError && (
          <Alert
            type="warning"
            showIcon
            style={{ marginTop: 10 }}
            message="已放宽匹配"
            description={`没有文档同时包含全部关键词，以下为部分匹配结果${
              searchMatchedTerms.length ? `（实际命中：${searchMatchedTerms.join('、')}）` : ''
            }。需要完整匹配请切换到「精确」模式。`}
          />
        )}

        {/* 结果区：加载中 / 请求失败 / 无结果 / 结果列表 */}
        <div style={{ marginTop: 12 }}>
          {searchError && (
            <Alert
              type={searchError.type}
              showIcon
              message={searchError.text}
              style={{ marginBottom: 12 }}
            />
          )}

          {searching ? (
            <div style={{ textAlign: 'center', padding: '32px 0' }}>
              <Spin tip="检索中…" />
            </div>
          ) : searchExecuted && !searchError && searchResults.length === 0 ? (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={searchEmptyDescription} />
          ) : searchResults.length > 0 ? (
            <>
              <List
                dataSource={searchResults}
                split
                renderItem={(item) => (
                  <List.Item
                    className="search-hit-item"
                    style={{ cursor: 'pointer', padding: '10px 8px' }}
                    onClick={() => handleOpenSearchResult(item)}
                  >
                    <div style={{ width: '100%' }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                        <Tooltip title={item.filename || `文档 ${item.doc_id}`}>
                          <span className="search-hit-title">{item.filename || `文档 ${item.doc_id}`}</span>
                        </Tooltip>
                        <Tag color="blue">doc #{item.doc_id}</Tag>
                        <Tag>得分 {Number(item.score || 0).toFixed(2)}</Tag>
                      </div>
                      {item.title && item.title !== item.filename && (
                        <div style={{ fontSize: 13, color: 'var(--pm-text-2)', marginTop: 2 }}>
                          标题：{item.title}
                        </div>
                      )}
                      {item.snippet && (
                        <div
                          className="search-snippet"
                          dangerouslySetInnerHTML={{ __html: item.snippet }}
                        />
                      )}
                    </div>
                  </List.Item>
                )}
              />
              <div style={{ display: 'flex', justifyContent: 'flex-end', marginTop: 12 }}>
                <Pagination
                  size="small"
                  current={searchPage}
                  pageSize={searchPageSize}
                  total={searchTotal}
                  showSizeChanger
                  pageSizeOptions={['10', '20', '30', '50']}
                  showTotal={(t) => `共 ${t} 条`}
                  onChange={(p, size) => {
                    if (size && size !== searchPageSize) {
                      setSearchPageSize(size);
                      runSearch(1, size);
                    } else {
                      runSearch(p);
                    }
                  }}
                />
              </div>
            </>
          ) : (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description={searchReady ? '输入关键词后回车检索' : '索引尚未就绪，请先构建索引'}
            />
          )}
        </div>
      </Drawer>
    </Layout>
  );
}

function App() {
  return (
    <AntApp>
      <MainApp />
    </AntApp>
  );
}

export default App;
