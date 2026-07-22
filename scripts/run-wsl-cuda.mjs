#!/usr/bin/env node

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binary = resolve(root, "src-tauri/target/release/biturbo");
const rebuild = process.argv.includes("--rebuild") || !existsSync(binary);
const env = {
  ...process.env,
  BITURBO_EMBED_EP: process.env.BITURBO_EMBED_EP || "auto",
  ORT_CUDA_VERSION: process.env.ORT_CUDA_VERSION || "12",
  LD_LIBRARY_PATH: [
    "/usr/lib/wsl/lib",
    "/usr/local/cuda/lib64",
    "/usr/lib/x86_64-linux-gnu",
    process.env.LD_LIBRARY_PATH,
  ].filter(Boolean).join(":"),
};

function run(command, args) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, { cwd: root, env, stdio: "inherit" });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolvePromise();
      else reject(new Error(`${command} exited with ${code ?? signal}`));
    });
  });
}

if (rebuild) {
  await run("npm", ["run", "build"]);
  await run("node", ["scripts/ensure-sidecar-placeholder.mjs"]);
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

await run(binary, []);
