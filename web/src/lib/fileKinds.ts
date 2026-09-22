/**
 * 文件类型 → 图标、图标配色、语法语言。
 *
 * 图标沿用 pi-web 的做法：把 Catppuccin VSCode 图标（`v1.26.0`，MIT，见
 * `web/public/icons/catppuccin/LICENSE`）当 alpha 蒙版用，颜色由我们自己的调色板
 * 令牌给——pi-web 统一用 `var(--text-dim)` 保持单色，这里按类型上色，文件树才能一眼
 * 分出「代码 / 配置 / 文档 / 数据」。
 *
 * 语言判定同样对齐 pi-web 的 `EXT_TO_LANGUAGE` + 文件名特例，但映射到
 * `highlight.js` 的语言 id（它的 `xml` 覆盖 html/xml，`ini` 覆盖 toml）。
 */

/** 缺省回退：未知类型用通用文件图标 + 次级文字色 */
const FALLBACK_ICON = '_file';
const FALLBACK_COLOR = 'var(--subtext0, currentColor)';

/** 扩展名 → 图标（对齐 pi-web 的 EXTENSION_ICONS） */
const EXTENSION_ICONS: Record<string, string> = {
  ts: 'typescript',
  tsx: 'typescript-react',
  js: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  jsx: 'javascript-react',
  py: 'python',
  json: 'json',
  jsonl: 'json',
  css: 'css',
  less: 'css',
  scss: 'sass',
  html: 'html',
  htm: 'html',
  md: 'markdown',
  mdx: 'markdown',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'toml',
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  fish: 'bash',
  rs: 'rust',
  go: 'go',
  sql: 'database',
  graphql: 'graphql',
  gql: 'graphql',
  tf: 'terraform',
  hcl: 'terraform',
  docx: 'ms-word',
  pdf: 'pdf',
  lock: 'lock',
};

/**
 * 图标 → 颜色：按文件的「角色」配色，而不是逐语言一色。
 *
 * 同一门语言的图标（如 typescript-react）共用一种色；调色板令牌由主题运行时注入，
 * 换主题自动跟随；令牌缺失时回退到当前文字色，不会出现无色图标。
 */
const ICON_COLORS: Record<string, string> = {
  typescript: 'var(--blue, currentColor)',
  'typescript-react': 'var(--blue, currentColor)',
  javascript: 'var(--yellow, currentColor)',
  'javascript-react': 'var(--yellow, currentColor)',
  python: 'var(--teal, currentColor)',
  rust: 'var(--peach, currentColor)',
  go: 'var(--sky, currentColor)',
  bash: 'var(--green, currentColor)',
  css: 'var(--mauve, currentColor)',
  sass: 'var(--pink, currentColor)',
  html: 'var(--maroon, currentColor)',
  markdown: 'var(--lavender, currentColor)',
  json: 'var(--sapphire, currentColor)',
  yaml: 'var(--sapphire, currentColor)',
  toml: 'var(--sapphire, currentColor)',
  database: 'var(--sapphire, currentColor)',
  graphql: 'var(--pink, currentColor)',
  terraform: 'var(--mauve, currentColor)',
  docker: 'var(--blue, currentColor)',
  git: 'var(--peach, currentColor)',
  env: 'var(--green, currentColor)',
  config: 'var(--overlay1, currentColor)',
  lock: 'var(--overlay1, currentColor)',
  'npm-lock': 'var(--overlay1, currentColor)',
  'bun-lock': 'var(--overlay1, currentColor)',
  next: 'var(--overlay1, currentColor)',
  eslint: 'var(--mauve, currentColor)',
  pdf: 'var(--maroon, currentColor)',
  'ms-word': 'var(--blue, currentColor)',
};

/** 具体文件名 → 图标（对齐 pi-web 的 getSpecialFileIcon） */
function specialIcon(name: string): string | undefined {
  if (name === 'dockerfile' || name.startsWith('dockerfile.')) return 'docker';
  if (name === '.env' || name.startsWith('.env.')) return 'env';
  if (['.gitignore', '.gitattributes', '.gitmodules'].includes(name)) return 'git';
  if (['.zshrc', '.bashrc', '.bash_profile', '.zprofile', '.profile', '.bash_aliases'].includes(name)) return 'bash';
  if (name === '.editorconfig' || name === '.npmrc' || name === '.prettierrc') return 'config';
  if (name === 'package-lock.json') return 'npm-lock';
  if (name === 'bun.lock') return 'bun-lock';
  if (name.startsWith('next.config.')) return 'next';
  if (
    ['.eslintrc', '.eslintrc.js', '.eslintrc.json', '.eslintrc.yml', 'eslint.config.mjs', 'eslint.config.js'].includes(
      name,
    )
  ) {
    return 'eslint';
  }
  if (['yarn.lock', 'pnpm-lock.yaml', 'cargo.lock'].includes(name)) return 'lock';
  if (/\.config\.(ts|js|mjs|cjs)$/.test(name)) return 'config';
  return undefined;
}

/** 扩展名 → highlight.js 语言 id（`plaintext` 表示不高亮） */
const EXTENSION_LANGUAGES: Record<string, string> = {
  ts: 'typescript',
  mts: 'typescript',
  cts: 'typescript',
  tsx: 'typescript',
  js: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  jsx: 'javascript',
  py: 'python',
  rb: 'ruby',
  go: 'go',
  rs: 'rust',
  java: 'java',
  kt: 'kotlin',
  swift: 'swift',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  cc: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  html: 'xml',
  htm: 'xml',
  xml: 'xml',
  svg: 'xml',
  css: 'css',
  scss: 'scss',
  less: 'less',
  json: 'json',
  jsonl: 'json',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'ini',
  ini: 'ini',
  conf: 'ini',
  md: 'markdown',
  mdx: 'markdown',
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  fish: 'bash',
  sql: 'sql',
  graphql: 'graphql',
  gql: 'graphql',
  dockerfile: 'dockerfile',
  env: 'bash',
  gitignore: 'bash',
  diff: 'diff',
  patch: 'diff',
};

/** 树里一项的呈现信息 */
export interface FileKind {
  /** `web/public/icons/catppuccin/<icon>.svg` 的基名 */
  icon: string;
  /** 图标的填充色（调色板令牌表达式） */
  color: string;
}

/** 按文件名取条目图标与配色；目录由调用方另行处理。 */
export function fileKind(name: string): FileKind {
  const lower = name.toLowerCase();
  const icon = specialIcon(lower) ?? EXTENSION_ICONS[lower.split('.').pop() ?? ''];
  if (!icon) return { icon: FALLBACK_ICON, color: FALLBACK_COLOR };
  return { icon, color: ICON_COLORS[icon] ?? FALLBACK_COLOR };
}

/** 按文件名判定 highlight.js 语言；未知一律 `plaintext`（不高亮）。 */
export function fileLanguage(name: string): string {
  const lower = name.split('/').pop()?.toLowerCase() ?? '';
  if (lower === 'dockerfile' || lower.startsWith('dockerfile.')) return 'dockerfile';
  if (lower === '.env' || lower.startsWith('.env.')) return 'bash';
  if (lower === 'makefile' || lower === 'gnumakefile') return 'makefile';
  if (['.gitignore', '.gitattributes', '.gitmodules', '.bashrc', '.zshrc', '.profile'].includes(lower)) {
    return 'bash';
  }
  return EXTENSION_LANGUAGES[lower.split('.').pop() ?? ''] ?? 'plaintext';
}
