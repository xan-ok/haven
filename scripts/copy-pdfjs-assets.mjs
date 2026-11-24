// FR-34：把 PDF.js 的 CMap 与标准字体复制到 public/pdfjs/。
//
// 这些资源用于中日韩字符与标准 14 字体的本地渲染，必须在应用内打包，
// 运行期不得从 CDN 加载。脚本在 predev / prebuild 时执行。

import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = join(projectRoot, "node_modules", "pdfjs-dist");
const targetRoot = join(projectRoot, "public", "pdfjs");
const dirs = ["cmaps", "standard_fonts"];

mkdirSync(targetRoot, { recursive: true });

for (const dir of dirs) {
  const source = join(sourceRoot, dir);
  if (!existsSync(source)) {
    console.error(`缺少 PDF.js 资源目录：${source}，请先执行 npm install。`);
    process.exit(1);
  }
  const target = join(targetRoot, dir);
  rmSync(target, { recursive: true, force: true });
  cpSync(source, target, { recursive: true });
}
