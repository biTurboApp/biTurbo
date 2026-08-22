// Creates a git tag matching the version in package.json and pushes it
// to trigger the release workflow. Usage: pnpm release
//
// Checks:
//   - Working tree must be clean (no uncommitted changes)
//   - Tag must not already exist remotely
//   - Current commit must be pushed to origin
import { readFileSync } from "node:fs";
import { execSync } from "node:child_process";

const pkg = JSON.parse(readFileSync("package.json", "utf8"));
const version = pkg.version;

if (!version) {
  console.error("No version found in package.json");
  process.exit(1);
}

const tag = `v${version}`;

function git(args) {
  return execSync(`git ${args}`, { encoding: "utf8", stdio: ["pipe", "pipe", "pipe"] }).trim();
}

// 1. Ensure clean working tree
const status = git("status --porcelain");
if (status) {
  console.error("Working tree is not clean. Commit or stash changes first.");
  console.error(status);
  process.exit(1);
}

// 2. Ensure current commit is pushed
const localBranch = git("rev-parse --abbrev-ref HEAD");
const localCommit = git("rev-parse HEAD");
let remoteCommit;
try {
  remoteCommit = git(`rev-parse origin/${localBranch}`);
} catch {
  console.error(`No remote branch found for "${localBranch}". Push your branch first.`);
  process.exit(1);
}

if (localCommit !== remoteCommit) {
  console.error(`Local commit ${localCommit.slice(0, 8)} does not match origin/${localBranch} (${remoteCommit.slice(0, 8)}).`);
  console.error("Push your changes first: git push");
  process.exit(1);
}

// 3. Check if tag already exists
let tagExists = false;
try {
  git(`rev-parse ${tag}`);
  tagExists = true;
} catch {
  // Tag doesn't exist — good
}

if (tagExists) {
  console.error(`Tag ${tag} already exists. Bump the version in package.json first.`);
  process.exit(1);
}

// 4. Create and push the tag
console.log(`Creating tag ${tag} for version ${version}...`);
execSync(`git tag ${tag}`, { stdio: "inherit" });

console.log(`Pushing ${tag} to origin...`);
execSync(`git push origin ${tag}`, { stdio: "inherit" });

console.log(`\nDone! The release workflow is now running.`);
console.log(`Check progress: https://github.com/${git("remote get-url origin").replace(/\.git$/, "").replace(/^https:\/\/github\.com\//, "")}/actions`);
