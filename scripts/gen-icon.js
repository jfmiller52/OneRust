const sharp = require("sharp");
const fs = require("fs");
const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512">
  <rect width="512" height="512" rx="96" fill="#0f1419"/>
  <rect x="64" y="64" width="384" height="384" rx="48" fill="none" stroke="#3d9bfd" stroke-width="28"/>
  <text x="256" y="300" text-anchor="middle" font-family="Arial" font-size="160" font-weight="700" fill="#e8eef5">OR</text>
</svg>`;

async function main() {
  fs.mkdirSync("src-tauri/icons", { recursive: true });
  await sharp(Buffer.from(svg)).png().toFile("src-tauri/icons/icon.png");
  await sharp(Buffer.from(svg)).resize(32, 32).png().toFile("src-tauri/icons/32x32.png");
  await sharp(Buffer.from(svg)).resize(128, 128).png().toFile("src-tauri/icons/128x128.png");
  // Minimal ICO: reuse PNG as icon.ico fallback — Tauri accepts PNG listed; copy for ico name
  fs.copyFileSync("src-tauri/icons/icon.png", "src-tauri/icons/icon.ico");
  console.log("icons ok");
}
main().catch((e) => {
  console.error(e);
  process.exit(1);
});
