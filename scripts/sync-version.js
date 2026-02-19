import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const rootDir = path.resolve(__dirname, '..');
const packageJsonPath = path.join(rootDir, 'package.json');
const tauriConfPath = path.join(rootDir, 'src-tauri', 'tauri.conf.json');
const cargoTomlPath = path.join(rootDir, 'src-tauri', 'Cargo.toml');

// Read package.json
const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, 'utf-8'));
const version = packageJson.version;

console.log(`Syncing version: ${version}`);

// Update tauri.conf.json
try {
    const tauriConf = JSON.parse(fs.readFileSync(tauriConfPath, 'utf-8'));
    if (tauriConf.version !== version) {
        tauriConf.version = version;
        fs.writeFileSync(tauriConfPath, JSON.stringify(tauriConf, null, 2) + '\n');
        console.log(`Updated tauri.conf.json to ${version}`);
    } else {
        console.log(`tauri.conf.json already at ${version}`);
    }
} catch (error) {
    console.error('Error updating tauri.conf.json:', error);
    process.exit(1);
}

// Update Cargo.toml
try {
    let cargoToml = fs.readFileSync(cargoTomlPath, 'utf-8');
    const versionRegex = /^version = ".*"/m;
    if (versionRegex.test(cargoToml)) {
        const currentCargoVersionMatch = cargoToml.match(versionRegex);
        if (currentCargoVersionMatch && currentCargoVersionMatch[0] !== `version = "${version}"`) {
            cargoToml = cargoToml.replace(versionRegex, `version = "${version}"`);
            fs.writeFileSync(cargoTomlPath, cargoToml);
            console.log(`Updated Cargo.toml to ${version}`);
        } else {
            console.log(`Cargo.toml already at ${version}`);
        }
    } else {
        console.error('Could not find version in Cargo.toml');
        process.exit(1);
    }
} catch (error) {
    console.error('Error updating Cargo.toml:', error);
    process.exit(1);
}
