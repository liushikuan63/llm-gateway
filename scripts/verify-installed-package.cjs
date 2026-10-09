// Verify installer payload bytes against this checkout's release and resources.
// Tauri changes one fixed bundle-type string in the EXE; no other differences are allowed.
const fs = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");

const repository = path.resolve(__dirname, "..");
const release = path.join(repository, "src-tauri", "target", "release");
const sourceResources = path.join(repository, "src-tauri", "resources", "mihomo");
const unknownMarker = Buffer.from("__TAURI_BUNDLE_TYPE_VAR_UNK", "ascii");
const bundleMarkers = {
  nsis: Buffer.from("__TAURI_BUNDLE_TYPE_VAR_NSS", "ascii"),
  msi: Buffer.from("__TAURI_BUNDLE_TYPE_VAR_MSI", "ascii"),
};

function requireCondition(condition, message) {
  if (!condition) throw new Error(message);
}

function parseArguments(args) {
  const usage = "Usage: node scripts/verify-installed-package.cjs --bundle nsis|msi --dir <absolute-payload-directory>";
  requireCondition(args.length === 4, usage);
  const options = {};
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    requireCondition(key === "--bundle" || key === "--dir", usage);
    requireCondition(!Object.hasOwn(options, key), usage);
    options[key] = args[index + 1];
  }
  requireCondition(Object.hasOwn(bundleMarkers, options["--bundle"]), usage);
  requireCondition(typeof options["--dir"] === "string" && path.isAbsolute(options["--dir"]), usage);
  return { bundle: options["--bundle"], directory: options["--dir"] };
}

function readRegularFile(filename, label) {
  requireCondition(fs.lstatSync(filename).isFile(), `${label} must be a regular file`);
  return fs.readFileSync(filename);
}

function fileList(directory, prefix = "") {
  requireCondition(fs.lstatSync(directory).isDirectory(), "Resource root must be a regular directory");
  const files = [];
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) files.push(...fileList(path.join(directory, entry.name), relative));
    else {
      requireCondition(entry.isFile(), `Unsupported resource entry: ${relative}`);
      files.push(relative);
    }
  }
  return files.sort();
}

function verifyPayload({ bundle, directory }) {
  requireCondition(fs.lstatSync(directory).isDirectory(), "Payload root must be a regular directory");
  const builtExe = readRegularFile(path.join(release, "llm-gateway.exe"), "Release EXE");
  const payloadExe = readRegularFile(path.join(directory, "llm-gateway.exe"), "Payload EXE");
  requireCondition(builtExe.length === payloadExe.length, "Payload EXE length differs from release");
  const offset = builtExe.indexOf(unknownMarker);
  requireCondition(offset >= 0 && builtExe.indexOf(unknownMarker, offset + 1) === -1,
    "Release EXE must contain exactly one unknown Tauri bundle marker");
  requireCondition(payloadExe.subarray(offset, offset + unknownMarker.length).equals(bundleMarkers[bundle]),
    `Payload EXE must contain the ${bundle} marker at the release marker offset`);
  const normalized = Buffer.from(payloadExe);
  unknownMarker.copy(normalized, offset);
  requireCondition(normalized.equals(builtExe), "Payload EXE differs beyond the single permitted Tauri bundle marker");

  const payloadDll = path.join(directory, "llm_gateway_lib.dll");
  let dllPresent = false;
  try { fs.lstatSync(payloadDll); dllPresent = true; }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  requireCondition(bundle !== "msi" || dllPresent, "MSI payload is missing llm_gateway_lib.dll");
  if (dllPresent) {
    const expected = readRegularFile(path.join(release, "llm_gateway_lib.dll"), "Release DLL");
    const actual = readRegularFile(payloadDll, "Payload DLL");
    requireCondition(actual.equals(expected), "Payload DLL differs from release");
  }

  const payloadResources = path.join(directory, "resources", "mihomo");
  const expectedFiles = fileList(sourceResources);
  const actualFiles = fileList(payloadResources);
  requireCondition(expectedFiles.length === 6, "Repository Mihomo resource set must contain exactly six files");
  requireCondition(JSON.stringify(actualFiles) === JSON.stringify(expectedFiles), "Payload Mihomo resource file list differs from repository");
  for (const relative of expectedFiles) {
    const expected = readRegularFile(path.join(sourceResources, relative), `Repository resource ${relative}`);
    const actual = readRegularFile(path.join(payloadResources, relative), `Payload resource ${relative}`);
    requireCondition(actual.equals(expected), `Payload resource differs from repository: ${relative}`);
  }
  return {
    verified: true,
    bundle,
    bundle_marker_offset: offset,
    normalized_exe_sha256: createHash("sha256").update(normalized).digest("hex"),
    dll_verified: dllPresent,
    resource_files_verified: expectedFiles.length,
  };
}

if (require.main === module) {
  try {
    console.log(`INSTALLED_PACKAGE_VERIFIED: ${JSON.stringify(verifyPayload(parseArguments(process.argv.slice(2))))}`);
  } catch (error) {
    console.error(`INSTALLED_PACKAGE_VERIFICATION_FAILED: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { parseArguments, verifyPayload };
