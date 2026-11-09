# Third-party components

First-party licensing is component-specific: the client SDK and OBS adapter use BSD-3-Clause, the Relay and gateway use AGPL-3.0-only, and protocol documents use Apache-2.0. See LICENSING.md and the component LICENSE files. Individual dependencies retain their own licenses. Release packages include a Cargo dependency inventory in dependencies.json and collected license/copyright notices in licenses/. The companion source archive includes locked Rust dependency sources in vendor/ with their original notices. The inventory includes dependencies for other target platforms; listing a dependency does not imply every binary links it.

The browser gateway embeds hls.js, Copyright (c) 2017 Dailymotion, Apache-2.0. Its notice is included under licenses/hls.js and its source/license are in scripts/vendor/hls.js.

The OBS module in the separate obs-periapsis repository is BSD-3-Clause. Retain its BSD license and attribution in binary distributions. Matching source is provided as a release artifact. The package does not include OBS Studio, Qt, Microsoft Visual C++ runtime or FFmpeg; those are separately installed components. The plugin reuses the OBS application's Qt runtime. No third-party license or ownership is replaced by the project's version number.
