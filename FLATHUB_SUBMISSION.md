# Submitting ClusterCut to Flathub

This guide outlines the steps to submit ClusterCut to Flathub.

## Prerequisites

1.  **GitHub Account**: You need a GitHub account to submit a pull request.
2.  **Fork Flathub Repo**: The modern way is to submit a new application via the [Flathub submission portal](https://github.com/flathub/flathub/wiki/App-Submission) or by creating a PR against `flathub/new-pr` repo which triggers the pipeline.*
    *Correction: You should create a new repository in your own execution environment with the manifest and then submit a PR to `flathub/new-pr`.*

## Submission Steps

1.  **Prepare the Repository**
    Create a new git repository (or a branch in your existing one) specifically for the Flathub submission. It should contain:
    - `com.keithvassallo.clustercut.yml` (The manifest)
    - `com.keithvassallo.clustercut.metainfo.xml`
    - `com.keithvassallo.clustercut.desktop`
    - Icons (e.g., `icon-512.png`)
    - `cargo-sources.json` and `node-sources.json` (generated via `just flatpak-flathub`)

2.  **Review the Manifest**
    The manifest `src-tauri/flatpak/com.keithvassallo.clustercut.yml` is already configured to point to the [ClusterCut repository](https://github.com/keithvassallomt/ClusterCut) with tag `v0.1.1`.
    
    *Configuration:*
    ```yaml
      - type: git
        url: https://github.com/keithvassallomt/ClusterCut.git
        tag: v0.1.1
    ```

    3.  **Test the Build**
    Run the verified build command:
    ```bash
    just flatpak
    ```

4.  **Submit to Flathub**
    - Go to [Flathub's New PR repository](https://github.com/flathub/new-pr).
    - Fork the repository.
    - Add your manifest and related files.
    - Submit a Pull Request.

## Important Notes

- **Metadata**: The `com.keithvassallo.clustercut.metainfo.xml` file is crucial. It provides the description, screenshots, and version info on Flathub.
- **Icons**: Ensure high-quality icons are included.
- **Network Access**: The manifest enables network access (`--share=network`) which is required for peer synchronization.

## Maintenance

After approval, you will get a repository under `https://github.com/flathub/com.keithvassallo.clustercut`. You will push updates there to trigger new builds.
