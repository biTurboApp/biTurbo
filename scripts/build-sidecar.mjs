// Builds the biturbo-mcp sidecar binary and copies it to src-tauri/binaries/
// with the target-triple suffix that Tauri expects for externalBin.
//
// Tauri's build.rs validates externalBin paths at compile time, so we create
// a placeholder file before building, then overwrite it with the real binary.
import { execSync } from "node:child_process";
import { cpSync, mkdirSync, existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = process.cwd();
const srcTauri = join(root, "src-tauri");
const binariesDir = join(srcTauri, "binaries");

// Get the host target triple (e.g. x86_64-pc-windows-msvc, aarch64-apple-darwin)
const targetTriple = execSync("rustc -vV", { encoding: "utf8" })
  .match(/host:\s*(.+)/)?.[1]
  .trim();

if (!targetTriple) {
  console.error("Could not determine rustc host target triple");
  process.exit(1);
}

const isWindows = process.platform === "win32";
const ext = isWindows ? ".exe" : "";
const dst = join(binariesDir, `biturbo-mcp-${targetTriple}${ext}`);

// Create placeholder so Tauri's build.rs doesn't fail on the externalBin check.
mkdirSync(binariesDir, { recursive: true });
writeFileSync(dst, "placeholder");

console.log(`Building biturbo-mcp for ${targetTriple}...`);

execSync(
  `cargo build --manifest-path "${join(srcTauri, "Cargo.toml")}" --bin biturbo-mcp --release`,
  { stdio: "inherit" },
);

const src = join(srcTauri, "target", "release", `biturbo-mcp${ext}`);

if (!existsSync(src)) {
  console.error(`Built binary not found at ${src}`);
  process.exit(1);
}

cpSync(src, dst);
console.log(`Sidecar copied to ${dst}`);
