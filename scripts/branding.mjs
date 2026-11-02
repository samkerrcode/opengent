import { copyFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const assets = join(root, "docs/assets");
const icons = join(root, "src-tauri/icons");
const product = JSON.parse(readFileSync(join(root, "product.json"), "utf8"));
const escape = (value) => value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");
const name = escape(product.displayName);
const mark = readFileSync(join(assets, "logo.svg"), "utf8")
  .replace(/<svg[^>]*>|<\/svg>|<title>.*?<\/title>/gs, "").trim();
const inkscape = process.env.INKSCAPE_BIN ?? (process.platform === "darwin" && existsSync("/Applications/Inkscape.app/Contents/MacOS/inkscape")
  ? "/Applications/Inkscape.app/Contents/MacOS/inkscape" : "inkscape");
execFileSync(inkscape, ["--version"], { stdio: "pipe" });

const svg = (width, height, title, content) => `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" fill="none">
  <title>${title}</title>
${content}
</svg>
`;
const font = 'font-family="Arial, Helvetica, sans-serif"';
const temp = mkdtempSync(join(tmpdir(), "opengent-brand-"));
try {
  for (const [file, foreground, accent] of [
    ["logo-horizontal.svg", "#15181e", "#2f6fed"],
    ["logo-horizontal-dark.svg", "#e9ebf0", "#6ea8ff"],
  ]) {
    const source = join(temp, file);
    writeFileSync(source, svg(640, 192, name, `
  <g transform="translate(8 16) scale(.625)">${mark.replaceAll("#2f6fed", accent)}</g>
  <text x="194" y="124" ${font} font-size="94" font-weight="500" letter-spacing="-2" fill="${foreground}">${name}</text>`));
    execFileSync(inkscape, [source, "--export-text-to-path", "--export-plain-svg", `--export-filename=${join(assets, file)}`], { stdio: "pipe" });
  }

  const social = join(temp, "social-preview.svg");
  writeFileSync(social, svg(1280, 640, `${name} — desktop AI agents, on your terms`, `
  <defs>
    <radialGradient id="glow" cx="1" cy="0" r="1">
      <stop stop-color="#1b3152"/><stop offset="1" stop-color="#101216"/>
    </radialGradient>
  </defs>
  <rect width="1280" height="640" fill="#101216"/>
  <rect x="1" y="1" width="1278" height="638" rx="28" fill="url(#glow)" stroke="#283347"/>
  <g transform="translate(76 70) scale(.5)">${mark.replaceAll("#2f6fed", "#6ea8ff")}</g>
  <text x="232" y="158" ${font} font-size="76" font-weight="500" letter-spacing="-2" fill="#e9ebf0">${name}</text>
  <text x="88" y="307" ${font} font-size="58" font-weight="600" letter-spacing="-1.5" fill="#e9ebf0">Your AI agents. Your rules.</text>
  <text x="90" y="376" ${font} font-size="29" fill="#aeb8c8">Any model. Only the tools you allow.</text>
  <text x="90" y="420" ${font} font-size="29" fill="#aeb8c8">Every step in plain sight.</text>
  <path d="M90 489h1100" stroke="#344157"/>
  <text x="90" y="548" ${font} font-size="21" letter-spacing="1" fill="#aeb8c8">DESKTOP AI WORKSPACE</text>
  <text x="1190" y="548" text-anchor="end" ${font} font-size="21" fill="#6ea8ff">Open source · Local or cloud models</text>`));
  execFileSync(inkscape, [social, "--export-text-to-path", "--export-plain-svg", `--export-filename=${join(assets, "social-preview.svg")}`], { stdio: "pipe" });
  execFileSync(inkscape, [join(assets, "social-preview.svg"), `--export-filename=${join(assets, "social-preview.png")}`], { stdio: "pipe" });

  writeFileSync(join(icons, "source.svg"), svg(1024, 1024, `${name} app icon`, `
  <defs>
    <linearGradient id="surface" x1="0" y1="0" x2="1" y2="1">
      <stop stop-color="#202939"/><stop offset="1" stop-color="#101216"/>
    </linearGradient>
    <linearGradient id="blue" gradientUnits="userSpaceOnUse" x1="0" y1="22" x2="0" y2="234">
      <stop stop-color="#6ea8ff"/><stop offset="1" stop-color="#2f6fed"/>
    </linearGradient>
  </defs>
  <rect x="64" y="64" width="896" height="896" rx="204" fill="url(#surface)" stroke="#344157" stroke-width="2"/>
  <g transform="translate(160 160) scale(2.75)">${mark.replaceAll("#2f6fed", "url(#blue)")}</g>`));

  const cli = join(root, "node_modules/@tauri-apps/cli/tauri.js");
  const output = join(temp, "icons");
  execFileSync(process.execPath, [cli, "icon", join(icons, "source.svg"), "--output", output], { stdio: "pipe" });
  for (const file of readdirSync(output)) {
    if (/\.(png|ico|icns)$/.test(file)) copyFileSync(join(output, file), join(icons, file));
  }
  const icnsPath = join(icons, "icon.icns");
  const icns = readFileSync(icnsPath);
  const entries = [];
  for (let offset = 8; offset < icns.length;) {
    const length = icns.readUInt32BE(offset + 4);
    if (length < 8 || offset + length > icns.length) throw new Error("Invalid generated ICNS entry");
    entries.push(icns.subarray(offset, offset + length));
    offset += length;
  }
  entries.sort((a, b) => Buffer.compare(a.subarray(0, 4), b.subarray(0, 4)));
  writeFileSync(icnsPath, Buffer.concat([icns.subarray(0, 8), ...entries]));
  console.log("Generated horizontal logos, social preview, and desktop icons from docs/assets/logo.svg.");
} finally {
  rmSync(temp, { recursive: true, force: true });
}
