import cssIcon from "../../assets/file-icons/css.svg";
import databaseIcon from "../../assets/file-icons/database.svg";
import dockerIcon from "../../assets/file-icons/docker.svg";
import fileIcon from "../../assets/file-icons/file.svg";
import folderIcon from "../../assets/file-icons/folder.svg";
import folderConfigIcon from "../../assets/file-icons/folder-config.svg";
import folderConfigOpenIcon from "../../assets/file-icons/folder-config-open.svg";
import folderDocsIcon from "../../assets/file-icons/folder-docs.svg";
import folderDocsOpenIcon from "../../assets/file-icons/folder-docs-open.svg";
import folderGitIcon from "../../assets/file-icons/folder-git.svg";
import folderGitOpenIcon from "../../assets/file-icons/folder-git-open.svg";
import folderNodeIcon from "../../assets/file-icons/folder-node.svg";
import folderNodeOpenIcon from "../../assets/file-icons/folder-node-open.svg";
import folderOpenIcon from "../../assets/file-icons/folder-open.svg";
import folderRustIcon from "../../assets/file-icons/folder-rust.svg";
import folderRustOpenIcon from "../../assets/file-icons/folder-rust-open.svg";
import folderSrcIcon from "../../assets/file-icons/folder-src.svg";
import folderSrcOpenIcon from "../../assets/file-icons/folder-src-open.svg";
import folderTestIcon from "../../assets/file-icons/folder-test.svg";
import folderTestOpenIcon from "../../assets/file-icons/folder-test-open.svg";
import folderUiIcon from "../../assets/file-icons/folder-ui.svg";
import folderUiOpenIcon from "../../assets/file-icons/folder-ui-open.svg";
import gitIcon from "../../assets/file-icons/git.svg";
import htmlIcon from "../../assets/file-icons/html.svg";
import imageIcon from "../../assets/file-icons/image.svg";
import javascriptIcon from "../../assets/file-icons/javascript.svg";
import jsonIcon from "../../assets/file-icons/json.svg";
import licenseIcon from "../../assets/file-icons/license.svg";
import markdownIcon from "../../assets/file-icons/markdown.svg";
import npmIcon from "../../assets/file-icons/npm.svg";
import pythonIcon from "../../assets/file-icons/python.svg";
import reactIcon from "../../assets/file-icons/react.svg";
import reactTsIcon from "../../assets/file-icons/react_ts.svg";
import readmeIcon from "../../assets/file-icons/readme.svg";
import rustIcon from "../../assets/file-icons/rust.svg";
import settingsIcon from "../../assets/file-icons/settings.svg";
import svgIcon from "../../assets/file-icons/svg.svg";
import tauriIcon from "../../assets/file-icons/tauri.svg";
import tomlIcon from "../../assets/file-icons/toml.svg";
import typescriptIcon from "../../assets/file-icons/typescript.svg";
import typescriptDefIcon from "../../assets/file-icons/typescript-def.svg";
import viteIcon from "../../assets/file-icons/vite.svg";
import vitestIcon from "../../assets/file-icons/vitest.svg";
import yamlIcon from "../../assets/file-icons/yaml.svg";

const exactFileIcons: Record<string, string> = {
  "cargo.lock": rustIcon,
  "cargo.toml": rustIcon,
  "dockerfile": dockerIcon,
  ".dockerignore": dockerIcon,
  ".env": settingsIcon,
  ".env.local": settingsIcon,
  ".gitignore": gitIcon,
  ".gitattributes": gitIcon,
  "license": licenseIcon,
  "license.md": licenseIcon,
  "package-lock.json": npmIcon,
  "package.json": npmIcon,
  "pnpm-lock.yaml": npmIcon,
  "readme": readmeIcon,
  "readme.md": readmeIcon,
  "tauri.conf.json": tauriIcon,
  "tsconfig.json": typescriptIcon,
  "vite.config.js": viteIcon,
  "vite.config.ts": viteIcon,
  "vitest.config.js": vitestIcon,
  "vitest.config.ts": vitestIcon,
  "yarn.lock": npmIcon,
};

const extensionIcons: Record<string, string> = {
  css: cssIcon,
  dts: typescriptDefIcon,
  htm: htmlIcon,
  html: htmlIcon,
  jpeg: imageIcon,
  jpg: imageIcon,
  js: javascriptIcon,
  json: jsonIcon,
  jsx: reactIcon,
  lock: settingsIcon,
  md: markdownIcon,
  mjs: javascriptIcon,
  png: imageIcon,
  py: pythonIcon,
  rs: rustIcon,
  sqlite: databaseIcon,
  svg: svgIcon,
  toml: tomlIcon,
  ts: typescriptIcon,
  tsx: reactTsIcon,
  webp: imageIcon,
  yaml: yamlIcon,
  yml: yamlIcon,
};

const folderIcons: Record<string, { closed: string; open: string }> = {
  ".git": { closed: folderGitIcon, open: folderGitOpenIcon },
  ".github": { closed: folderGitIcon, open: folderGitOpenIcon },
  ".vscode": { closed: folderConfigIcon, open: folderConfigOpenIcon },
  "config": { closed: folderConfigIcon, open: folderConfigOpenIcon },
  "configs": { closed: folderConfigIcon, open: folderConfigOpenIcon },
  "crates": { closed: folderRustIcon, open: folderRustOpenIcon },
  "docs": { closed: folderDocsIcon, open: folderDocsOpenIcon },
  "node_modules": { closed: folderNodeIcon, open: folderNodeOpenIcon },
  "src": { closed: folderSrcIcon, open: folderSrcOpenIcon },
  "src-tauri": { closed: folderRustIcon, open: folderRustOpenIcon },
  "target": { closed: folderRustIcon, open: folderRustOpenIcon },
  "test": { closed: folderTestIcon, open: folderTestOpenIcon },
  "tests": { closed: folderTestIcon, open: folderTestOpenIcon },
  "ui": { closed: folderUiIcon, open: folderUiOpenIcon },
};

export function getFileIcon(path: string) {
  const fileName = path.split("/").pop()?.toLowerCase() ?? path.toLowerCase();
  const exact = exactFileIcons[fileName];
  if (exact) return exact;

  const extension = fileName.split(".").pop() ?? "";
  return extensionIcons[extension] ?? fileIcon;
}

export function getFolderIcon(name: string, expanded: boolean) {
  const key = name.toLowerCase();
  const match = folderIcons[key];
  if (match) return expanded ? match.open : match.closed;
  return expanded ? folderOpenIcon : folderIcon;
}
