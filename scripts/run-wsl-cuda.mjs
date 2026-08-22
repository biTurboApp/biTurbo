#!/usr/bin/env node
/**
 * Run an optimized biTurbo desktop binary on WSL with CUDA embeds.
 *
 * Default: start the existing release binary (no rebuild).
 * Rebuild when missing, sources are newer, or forced:
 *   biturbo --rebuild
 *   BITURBO_FORCE_REBUILD=1 npm run tauri:run:wsl:cuda
 *
 * Env: VITE_UI_ZOOM (default 1.75)
 */
import { spawn } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const zoom = process.env.VITE_UI_ZOOM || "1.75";
const bin = resolve(root, "src-tauri/target/release/biturbo");
const distIndex = resolve(root, "dist/index.html");

const forceRebuild =
  process.env.BITURBO_FORCE_REBUILD === "1" ||
  process.argv.includes("--rebuild") ||
  process.argv.includes("-f");

const pathParts = ["/usr/local/cuda/bin", process.env.PATH || ""].filter(Boolean);
const libParts = [
  "/usr/lib/wsl/lib",
  resolve(process.env.HOME || "", ".local/lib/biturbo"),
  "/usr/local/cuda/lib64",
  "/usr/lib/x86_64-linux-gnu",
  process.env.LD_LIBRARY_PATH || "",
].filter(Boolean);

const env = {
  ...process.env,
  VITE_UI_ZOOM: zoom,
  PATH: pathParts.join(":"),
  LD_LIBRARY_PATH: libParts.join(":"),
  ORT_CUDA_VERSION: process.env.ORT_CUDA_VERSION || "12",
};

function run(cmd, args) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(cmd, args, {
      cwd: root,
      env,
      stdio: "inherit",
      shell: false,
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) resolvePromise();
      else reject(new Error(`${cmd} ${args.join(" ")} exited ${code}`));
    });
  });
}

function mtime(path) {
  try {
    return statSync(path).mtimeMs;
  } catch {
    return 0;
  }
}

function newestUnder(dir, exts, skip = new Set()) {
  let newest = 0;
  if (!existsSync(dir)) return newest;
  const stack = [dir];
  while (stack.length) {
    const cur = stack.pop();
    const base = cur.split(/[/\\]/).pop();
    if (skip.has(base)) continue;
    let entries;
    try {
      entries = readdirSync(cur, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const ent of entries) {
      const p = join(cur, ent.name);
      if (ent.isDirectory()) {
        if (!skip.has(ent.name)) stack.push(p);
        continue;
      }
      if (exts.some((e) => ent.name.endsWith(e))) {
        newest = Math.max(newest, mtime(p));
      }
    }
  }
  return newest;
}

function needsRebuild() {
  if (forceRebuild) return "forced (--rebuild / BITURBO_FORCE_REBUILD=1)";
  if (!existsSync(bin)) return "missing release binary";
  if (!existsSync(distIndex)) return "missing frontend dist";

  const binTime = mtime(bin);
  const frontendNewest = Math.max(
    newestUnder(resolve(root, "src"), [".ts", ".tsx", ".css", ".html"]),
    mtime(resolve(root, "index.html")),
    mtime(resolve(root, "package.json")),
    mtime(resolve(root, "vite.config.ts")),
    mtime(resolve(root, "tailwind.config.js")),
    mtime(resolve(root, "tailwind.config.ts")),
  );
  if (frontendNewest > mtime(distIndex)) return "frontend sources newer than dist";
  // Tauri embeds dist at compile time — rebuild Rust if dist or Rust sources changed.
  const rustNewest = Math.max(
    newestUnder(resolve(root, "src-tauri/src"), [".rs"]),
    mtime(resolve(root, "src-tauri/Cargo.toml")),
    mtime(resolve(root, "src-tauri/tauri.conf.json")),
    mtime(distIndex),
  );
  if (rustNewest > binTime) return "Rust/frontend artifacts newer than binary";
  return null;
}

async function rebuild() {
  console.log(`>> UI zoom VITE_UI_ZOOM=${zoom}`);
  console.log(">> Building optimized frontend…");
  await run("npm", ["run", "build"]);
  await run("node", ["scripts/ensure-sidecar-placeholder.mjs"]);
  console.log(">> Building release biturbo (--features cuda)…");
  await run("cargo", [
    "build",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "--release",
    "--features",
    "cuda",
    "--bin",
    "biturbo",
  ]);
}

async function main() {
  const reason = needsRebuild();
  if (reason) {
    console.log(`>> Rebuild needed: ${reason}`);
    await rebuild();
  } else {
    console.log(">> Reusing existing release build (pass --rebuild to force)");
  }

  if (!existsSync(bin)) {
    throw new Error(`missing binary: ${bin}`);
  }

  console.log(`>> Running ${bin}`);
  await run(bin, []);
}

main().catch((err) => {
  console.error(err.message || err);
  process.exit(1);
});
