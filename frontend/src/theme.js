// 统一视觉 token：沉稳蓝灰的专业工具风（浅色）
// 所有散落的十六进制色值收敛到这里，App.js 只引用 CSS 变量。
// 结构上把「语义色」与「中性色」分开，未来加深色主题时只需替换变量值。

export const palette = {
  // 主色：低饱和钢蓝，保证白底小字对比度（≥ 4.5:1）
  primary: '#2E5C8A',
  primaryHover: '#3C6F9F',
  primaryActive: '#24496F',
  primarySoft: '#EBF2F9',
  primaryBorder: '#B8CDE1',

  // 中性灰阶
  bgPage: '#F4F6F8',
  bgContainer: '#FFFFFF',
  bgSubtle: '#F7F9FA',
  border: '#DCE2E8',
  borderLight: '#E8EDF2',
  text: '#1F2933',
  textSecondary: '#52606D',
  textTertiary: '#6B7785',
  textDisabled: '#A7B1BB',

  // 语义色
  success: '#276749',
  successBg: '#EAF5EF',
  successBorder: '#B7DEC9',
  warning: '#8A5B0B',
  warningBg: '#FFF8E6',
  warningBorder: '#F0D79B',
  warningText: '#7A5009',
  error: '#C0392B',
  errorBg: '#FDECEA',
  errorBorder: '#F5C2BC',
  info: '#2E5C8A',
  infoBg: '#EBF2F9',
  infoBorder: '#B8CDE1',
  // 「处理中」用偏青的钢蓝，与「已上传」的蓝区分，但保持同一色系
  processing: '#2F6F8F',
  processingBg: '#EAF3F7',
  processingBorder: '#B9D7E3',

  // 顶部工具条
  headerBg: '#22384A',
  headerText: '#FFFFFF',
  headerMuted: '#C7D5E2',

  // 检索命中 / 搜索结果高亮
  searchHitBg: '#FFF1E6',
  searchHitText: '#A8541D',
  markBg: '#FCE8B2',
  markActiveBg: '#E58B7A',
  markActiveText: '#FFFFFF',

  // 其它
  shadowCard: '0 1px 2px rgba(16, 24, 40, 0.06), 0 1px 3px rgba(16, 24, 40, 0.04)',
  fontFamily:
    "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'Microsoft YaHei', 'PingFang SC', 'Hiragino Sans GB', sans-serif",
  fontMono: "Consolas, Monaco, 'Courier New', monospace"
};

// antd v5 ConfigProvider 主题：只在 token / components 层配置，组件内不再写死颜色
export const pmTheme = {
  token: {
    colorPrimary: palette.primary,
    colorInfo: palette.info,
    colorSuccess: palette.success,
    colorWarning: palette.warning,
    colorError: palette.error,
    colorLink: palette.primary,
    colorTextBase: palette.text,
    colorBgBase: palette.bgContainer,
    colorBgLayout: palette.bgPage,
    colorBorder: palette.border,
    colorBorderSecondary: palette.borderLight,
    colorTextSecondary: palette.textSecondary,
    colorTextTertiary: palette.textTertiary,
    colorTextQuaternary: palette.textDisabled,
    fontFamily: palette.fontFamily,
    fontSize: 14,
    borderRadius: 6,
    borderRadiusLG: 8,
    controlHeight: 32,
    lineWidth: 1,
    wireframe: false,
    boxShadow: '0 6px 16px rgba(16, 24, 40, 0.10)',
    boxShadowSecondary: '0 3px 10px rgba(16, 24, 40, 0.08)',
    boxShadowTertiary: palette.shadowCard
  },
  components: {
    Layout: {
      headerBg: palette.headerBg,
      headerColor: palette.headerText,
      headerHeight: 64,
      headerPadding: '10px 20px',
      bodyBg: palette.bgPage
    },
    Button: {
      fontWeight: 500,
      primaryShadow: 'none',
      defaultShadow: 'none',
      dangerShadow: 'none',
      defaultBorderColor: palette.border,
      defaultColor: palette.textSecondary,
      textTextHoverBg: palette.primarySoft
    },
    Table: {
      headerBg: palette.bgSubtle,
      headerColor: palette.textSecondary,
      headerSplitColor: 'transparent',
      borderColor: palette.borderLight,
      rowHoverBg: palette.primarySoft,
      rowSelectedBg: palette.primarySoft,
      rowSelectedHoverBg: '#E2ECF5',
      cellPaddingBlock: 10,
      cellPaddingInline: 12,
      cellFontSize: 13
    },
    Card: {
      paddingLG: 16,
      headerFontSize: 14
    },
    Modal: {
      titleFontSize: 15,
      headerBg: palette.bgContainer,
      contentBg: palette.bgContainer
    },
    Drawer: {
      colorBgElevated: palette.bgContainer
    },
    Tabs: {
      itemColor: palette.textSecondary,
      itemSelectedColor: palette.primary,
      itemHoverColor: palette.primaryHover,
      inkBarColor: palette.primary
    },
    Pagination: {
      itemActiveBg: palette.primarySoft
    },
    Menu: {
      itemSelectedBg: palette.primarySoft,
      itemSelectedColor: palette.primary
    },
    Tag: {
      defaultBg: palette.bgSubtle,
      defaultColor: palette.textSecondary
    },
    Input: {
      activeShadow: '0 0 0 2px rgba(46, 92, 138, 0.12)'
    },
    Select: {
      optionSelectedBg: palette.primarySoft,
      optionSelectedColor: palette.primary
    },
    Progress: {
      remainingColor: palette.borderLight
    }
  }
};

const ROOT_VARS = `
  --pm-primary: ${palette.primary};
  --pm-primary-hover: ${palette.primaryHover};
  --pm-primary-active: ${palette.primaryActive};
  --pm-primary-soft: ${palette.primarySoft};
  --pm-primary-border: ${palette.primaryBorder};
  --pm-bg-page: ${palette.bgPage};
  --pm-bg-container: ${palette.bgContainer};
  --pm-bg-subtle: ${palette.bgSubtle};
  --pm-border: ${palette.border};
  --pm-border-light: ${palette.borderLight};
  --pm-text: ${palette.text};
  --pm-text-2: ${palette.textSecondary};
  --pm-text-3: ${palette.textTertiary};
  --pm-text-disabled: ${palette.textDisabled};
  --pm-success: ${palette.success};
  --pm-success-bg: ${palette.successBg};
  --pm-success-border: ${palette.successBorder};
  --pm-warning: ${palette.warning};
  --pm-warning-bg: ${palette.warningBg};
  --pm-warning-border: ${palette.warningBorder};
  --pm-warning-text: ${palette.warningText};
  --pm-error: ${palette.error};
  --pm-error-bg: ${palette.errorBg};
  --pm-error-border: ${palette.errorBorder};
  --pm-info: ${palette.info};
  --pm-info-bg: ${palette.infoBg};
  --pm-info-border: ${palette.infoBorder};
  --pm-processing: ${palette.processing};
  --pm-processing-bg: ${palette.processingBg};
  --pm-processing-border: ${palette.processingBorder};
  --pm-header-bg: ${palette.headerBg};
  --pm-header-text: ${palette.headerText};
  --pm-header-muted: ${palette.headerMuted};
  --pm-search-hit-bg: ${palette.searchHitBg};
  --pm-search-hit-text: ${palette.searchHitText};
  --pm-mark-bg: ${palette.markBg};
  --pm-mark-active-bg: ${palette.markActiveBg};
  --pm-mark-active-text: ${palette.markActiveText};
  --pm-selection: rgba(46, 92, 138, 0.18);
  --pm-shadow-card: ${palette.shadowCard};
  --pm-font-mono: ${palette.fontMono};
`;

const GLOBAL_CSS = `
*, *::before, *::after { box-sizing: border-box; }
html, body, #root { height: 100%; }
body {
  margin: 0;
  overflow: hidden;
  background: var(--pm-bg-page);
  color: var(--pm-text);
  font-family: ${palette.fontFamily};
  -webkit-font-smoothing: antialiased;
}
::-webkit-scrollbar { width: 10px; height: 10px; }
::-webkit-scrollbar-thumb {
  background: #C9D2DB;
  border-radius: 6px;
  border: 2px solid transparent;
  background-clip: padding-box;
}
::-webkit-scrollbar-thumb:hover { background: #AEB9C4; background-clip: padding-box; }
::-webkit-scrollbar-track { background: transparent; }

/* ---------- 顶部工具条 ---------- */
.pm-header {
  height: auto !important;
  line-height: 1.4 !important;
  display: flex;
  flex: 0 0 auto;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  padding: 10px 20px;
  background: var(--pm-header-bg);
}
.pm-header .ant-btn { box-shadow: none; }
.pm-header .ant-btn-default,
.pm-header .ant-btn-text,
.pm-header .ant-btn-dashed {
  background: rgba(255, 255, 255, 0.06);
  border-color: rgba(255, 255, 255, 0.22);
  color: #E4ECF3;
}
.pm-header .ant-btn-default:not(:disabled):hover,
.pm-header .ant-btn-text:not(:disabled):hover,
.pm-header .ant-btn-dashed:not(:disabled):hover {
  background: rgba(255, 255, 255, 0.14) !important;
  border-color: rgba(255, 255, 255, 0.4) !important;
  color: #FFFFFF !important;
}
.pm-header .ant-btn-primary {
  background: #E8F0F7;
  border-color: #E8F0F7;
  color: #1B3A57;
}
.pm-header .ant-btn-primary:not(:disabled):hover {
  background: #FFFFFF !important;
  border-color: #FFFFFF !important;
  color: #16324B !important;
}
.pm-header .ant-btn-dangerous {
  background: transparent;
  border-color: rgba(242, 184, 178, 0.45);
  color: #F2B8B2;
}
.pm-header .ant-btn-dangerous:not(:disabled):hover {
  background: rgba(192, 57, 43, 0.22) !important;
  border-color: #F2B8B2 !important;
  color: #FFE4E0 !important;
}
.pm-header .ant-btn:disabled,
.pm-header .ant-btn.ant-btn-disabled {
  background: rgba(255, 255, 255, 0.03) !important;
  border-color: rgba(255, 255, 255, 0.12) !important;
  color: rgba(255, 255, 255, 0.35) !important;
}
.pm-header .ant-btn-dashed { border-style: dashed; }
.pm-app-title {
  margin: 0;
  display: flex;
  align-items: baseline;
  gap: 8px;
  color: var(--pm-header-text);
  font-size: 18px;
  font-weight: 600;
  letter-spacing: 0.02em;
  white-space: nowrap;
  flex: 0 0 auto;
}
.pm-app-title .pm-title-sub {
  font-size: 12px;
  font-weight: 400;
  color: var(--pm-header-muted);
}
.pm-header-tools {
  margin-left: auto;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  min-width: 0;
}

/* ---------- 页面容器 ---------- */
.pm-page { width: 100%; max-width: 1880px; margin: 0 auto; flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; }

/* 固定顶部、仅列表滚动的整体布局 */
.pm-layout { height: 100vh; overflow: hidden; }
.pm-content { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; overflow: hidden; }
.pm-list { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; }
.pm-table-area { flex: 1 1 auto; min-height: 0; }
.pm-page > *:not(.pm-list) { flex: 0 0 auto; }
.pm-filter-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 12px;
  margin-bottom: 16px;
}
.pm-selection-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 12px;
  padding: 6px 12px;
  margin-bottom: 12px;
  background: var(--pm-primary-soft);
  border: 1px solid var(--pm-primary-border);
  border-radius: 6px;
  color: var(--pm-primary);
  font-size: 13px;
}

/* 批量重命名：冲突行高亮 + 新名输入框布局 */
.pm-row-conflict > td {
  background: #fdf1f0 !important;
}

.pm-rename-name {
  display: flex;
  gap: 4px;
  align-items: center;
}

/* 可拖拽列宽的表头 */.pm-col-title {
  position: relative;
  display: flex;
  align-items: center;
  padding-right: 10px;
}

.pm-col-resizer {
  position: absolute;
  top: -8px;
  right: -8px;
  bottom: -8px;
  width: 14px;
  cursor: col-resize;
  z-index: 2;
}

.pm-col-resizer::after {
  content: '';
  position: absolute;
  top: 20%;
  bottom: 20%;
  left: 6px;
  width: 2px;
  border-radius: 1px;
  background: transparent;
}

.pm-col-resizer:hover::after {
  background: var(--pm-primary);
}
.pm-cell-ellipsis {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pm-file-cell { display: flex; align-items: center; gap: 8px; min-width: 0; }
.pm-file-main { min-width: 0; flex: 1; }
.pm-file-name {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  cursor: pointer;
  color: var(--pm-text);
}
.pm-file-path {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 12px;
  color: var(--pm-text-3);
}

/* ---------- 大页容量（≥200 条/页）轻量单元格 ---------- */
/* 1000 行时每行的 antd Select/Switch/Tooltip/Tag 会把主线程阻塞几十秒，
   这里用原生控件替代，保持列宽与布局不变。 */
.pm-lite-check {
  width: 14px;
  height: 14px;
  margin: 0;
  vertical-align: middle;
  accent-color: var(--pm-primary);
  cursor: pointer;
}
.pm-lite-check:disabled { cursor: not-allowed; opacity: 0.5; }
.pm-lite-select {
  width: 78px;
  height: 24px;
  padding: 0 2px;
  font-size: 12px;
  color: var(--pm-text-2);
  background: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: pointer;
}
.pm-lite-select:hover:not(:disabled) { border-color: var(--pm-border); background: var(--pm-bg-container); }
.pm-lite-select:disabled { opacity: 0.5; cursor: not-allowed; }
.pm-status-pill {
  display: inline-block;
  padding: 0 8px;
  line-height: 20px;
  font-size: 12px;
  border: 1px solid transparent;
  border-radius: 4px;
  white-space: nowrap;
}
.pm-tag-chip {
  display: inline-block;
  margin-right: 4px;
  padding: 0 6px;
  line-height: 18px;
  font-size: 12px;
  border: 1px solid currentColor;
  border-radius: 4px;
  white-space: nowrap;
}
.pm-lite-actions { display: flex; align-items: center; gap: 2px; }
.pm-lite-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  padding: 0;
  font-size: 13px;
  color: var(--pm-text-2);
  background: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: pointer;
}
.pm-lite-btn:hover:not(:disabled) { color: var(--pm-primary); background: var(--pm-primary-soft); }
.pm-lite-btn-primary { color: var(--pm-primary); }
.pm-lite-btn-danger:hover:not(:disabled) { color: var(--pm-error); background: var(--pm-error-bg); }
.pm-lite-btn:disabled { color: var(--pm-text-disabled); cursor: not-allowed; }
.pm-modal-title {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  padding-right: 28px;
}
.pm-toolbar-wrap { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 8px; }

/* ---------- Markdown 预览 ---------- */
.md-preview { color: var(--pm-text); }
.md-preview > *:first-child { margin-top: 0; }
.md-preview h1, .md-preview h2, .md-preview h3,
.md-preview h4, .md-preview h5, .md-preview h6 {
  margin: 14px 0 8px;
  line-height: 1.4;
  scroll-margin-top: 4px;
  color: var(--pm-text);
}
.md-preview h1 { font-size: 1.55em; border-bottom: 1px solid var(--pm-border); padding-bottom: 6px; }
.md-preview h2 { font-size: 1.35em; border-bottom: 1px solid var(--pm-border-light); padding-bottom: 4px; }
.md-preview h3 { font-size: 1.18em; }
.md-preview h4, .md-preview h5, .md-preview h6 { font-size: 1.05em; }
.md-preview p { margin: 8px 0; }
.md-preview table {
  display: block;
  width: 100%;
  max-width: 100%;
  overflow-x: auto;
  border-collapse: collapse;
  margin: 10px 0;
}
.md-preview th, .md-preview td {
  border: 1px solid var(--pm-border);
  padding: 6px 10px;
  text-align: left;
  min-width: 88px;
  white-space: normal;
  vertical-align: top;
}
.md-preview th {
  background: var(--pm-bg-subtle);
  font-weight: 600;
  position: sticky;
  top: 0;
}
.md-preview tr:nth-child(2n) td { background: var(--pm-bg-subtle); }
.md-preview code {
  background: var(--pm-bg-subtle);
  border: 1px solid var(--pm-border-light);
  border-radius: 3px;
  padding: 0 5px;
  font-family: var(--pm-font-mono);
  font-size: 0.92em;
  white-space: nowrap;
}
.md-preview pre {
  background: var(--pm-bg-subtle);
  border: 1px solid var(--pm-border-light);
  border-radius: 6px;
  padding: 12px;
  overflow-x: auto;
  max-width: 100%;
}
.md-preview pre code {
  background: none;
  border: none;
  padding: 0;
  white-space: pre;
  font-size: 12.5px;
  line-height: 1.6;
}
.md-preview ul, .md-preview ol { padding-left: 24px; margin: 8px 0; }
.md-preview blockquote {
  border-left: 4px solid var(--pm-border);
  margin: 10px 0;
  padding: 2px 12px;
  color: var(--pm-text-2);
  background: var(--pm-bg-subtle);
}
.md-preview img { max-width: 100%; height: auto; }
.md-preview hr { border: none; border-top: 1px solid var(--pm-border-light); margin: 14px 0; }
.md-preview a { color: var(--pm-primary); }

/* 实时预览编辑器 */
.live-editor .cm-md-strong { font-weight: 600; }
.live-editor .cm-md-em { font-style: italic; }
.live-editor .cm-md-strike { text-decoration: line-through; }
.live-editor .cm-md-inline-code {
  background: var(--pm-bg-subtle);
  border: 1px solid var(--pm-border-light);
  border-radius: 3px;
  padding: 0 3px;
}
.live-editor .cm-md-link { color: var(--pm-primary); text-decoration: underline; }
.live-editor .cm-md-heading { font-weight: 600; }
.live-editor .cm-md-heading-1, .live-editor .cm-md-heading-2 { border-bottom: 1px solid var(--pm-border-light); }
.live-editor .cm-md-match { background: var(--pm-info-bg); }
.live-editor .cm-md-image { display: inline-block; vertical-align: top; max-width: 100%; line-height: 0; }
.live-editor .cm-md-image img { display: block; max-width: 100%; height: auto; padding: 6px 0; }
.live-editor .cm-md-image-loading {
  min-width: 200px;
  min-height: 120px;
  background: var(--pm-bg-subtle);
  border: 1px dashed var(--pm-border);
  border-radius: 4px;
}
.live-editor .cm-md-image-error { color: var(--pm-text-3); font-size: 12px; line-height: 1.6; }

/* 目录与检索命中 */
.md-toc-item {
  padding: 4px 8px;
  border-radius: 4px;
  cursor: pointer;
  font-size: 13px;
  line-height: 1.6;
  color: var(--pm-text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.md-toc-item:hover { background: var(--pm-primary-soft); color: var(--pm-primary); }

/* 预览页里的垂直分隔条：拖动调整目录 / 正文宽度 */
.pm-pane-splitter {
  flex: 0 0 8px;
  align-self: stretch;
  border-radius: 4px;
  cursor: col-resize;
  background: transparent;
  position: relative;
}
.pm-pane-splitter::after {
  content: '';
  position: absolute;
  top: 0;
  bottom: 0;
  left: 3px;
  width: 2px;
  border-radius: 1px;
  background: transparent;
}
.pm-pane-splitter:hover::after { background: var(--pm-primary); }
mark.md-search-hit {
  background: var(--pm-mark-bg);
  color: inherit;
  padding: 0 1px;
  border-radius: 2px;
}
mark.md-search-hit.md-search-active {
  background: var(--pm-mark-active-bg);
  color: var(--pm-mark-active-text);
}

/* 检索抽屉 */
.search-snippet {
  margin-top: 6px;
  color: var(--pm-text-2);
  font-size: 13px;
  line-height: 1.7;
  word-break: break-word;
}
.search-snippet b {
  color: var(--pm-search-hit-text);
  background: var(--pm-search-hit-bg);
  font-weight: 600;
  padding: 0 1px;
  border-radius: 2px;
}
.search-hit-title {
  display: inline-block;
  flex: 0 1 auto;
  min-width: 0;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: bottom;
  font-weight: 600;
  color: var(--pm-primary);
}
.search-hit-item { transition: background 0.15s ease; }
.search-hit-item:hover { background: var(--pm-bg-subtle); }

/* 窄屏微调：低于 1200px 时压缩内边距与标题 */
@media (max-width: 1199px) {
  .pm-header { padding: 8px 12px; gap: 8px; }
  .pm-app-title { font-size: 16px; }
  .pm-selection-bar { font-size: 12px; }
}
`;

let installed = false;

// 注入 :root 变量与全局样式；重复调用无副作用
export function installGlobalStyles() {
  if (installed) return;
  installed = true;
  const style = document.createElement('style');
  style.id = 'pm-global-styles';
  style.textContent = `:root {${ROOT_VARS}}\n${GLOBAL_CSS}`;
  document.head.appendChild(style);
}
