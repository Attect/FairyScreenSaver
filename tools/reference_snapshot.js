// Builds a standalone snapshot page for the upstream Fairy-DSH mascot so the
// Rust/Vulkan renderer can be compared against the original SVG + CSS pixel for
// pixel.  Run with:  node tools/reference_snapshot.js <refClientDir> <out.html>
'use strict';

const fs = require('fs');
const path = require('path');

const dir = process.argv[2];
const out = process.argv[3] || 'reference.html';
if (!dir) {
  console.error('usage: node reference_snapshot.js <refClientDir> <out.html>');
  process.exit(2);
}

const assets = require(path.join(dir, 'mascot-assets.js'));
const styleSrc = fs.readFileSync(path.join(dir, 'style.js'), 'utf8');

// The upstream stylesheet is a set of JS template literals; a section runs from
// its opening selector to the backtick that closes the literal.
const section = (startRe) => {
  const i = styleSrc.search(startRe);
  if (i < 0) return '';
  const rest = styleSrc.slice(i);
  const j = rest.indexOf('`');
  return j < 0 ? rest : rest.slice(0, j);
};

// Line 15/16 of the upstream stylesheet: theme variables + background host.
const themeVars = section(/html\[data-dsh-fairy-visual\],html\[data-dsh-fairy-visual\] body\{/);
const bgHost = section(/\.dsh-hdd-background-host\{/);
const darkGlows = section(/html\[data-dsh-fairy-visual\]\[data-dsh-fairy-theme="dark"\] \.dsh-hdd-glow-a\{/);

const html = `<!DOCTYPE html>
<html data-dsh-fairy-visual data-dsh-fairy-theme="dark">
<head>
<meta charset="utf-8">
<title>Fairy reference snapshot</title>
<style>
${themeVars}
${bgHost}
${darkGlows}
html, body { margin: 0; width: 100%; height: 100%; overflow: hidden; }
body > .dsh-hdd-background-host { position: fixed; inset: 0; }

/* Pin the mascot to the middle of the viewport at a known size so the
   screenshot lines up with the Vulkan render. */
.dsh-fairy-stage {
  position: fixed !important;
  left: 0 !important; top: 0 !important;
  width: 100vw !important; height: 100vh !important;
  display: flex !important; align-items: center !important;
  justify-content: center !important;
  padding: 0 !important;
  opacity: 1 !important; visibility: visible !important;
  transform: none !important;
}
.dsh-fairy-stage > [data-dsh-fairy-mascot-root="true"] {
  top: 0 !important;
  width: MIN_SIZEpx !important;
  height: MIN_SIZEpx !important;
}

/* The upstream runtime drives eye breathing from one clock and disables the CSS
   timelines; here we freeze every timeline at its resting frame so a single
   screenshot is deterministic and directly comparable. */
[data-dsh-fairy-mascot-root="true"] * { animation-play-state: paused !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-corners { animation: none !important; transform: none !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-sclera { animation: none !important; transform: scale(.985) !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-layer-three { animation: none !important; transform: scale(1) !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-layer-two { animation: none !important; transform: scale(1) !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-layer-one { animation: none !important; transform: scale(1) !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-lash-pulse-wave { animation: none !important; transform: scale(1.35) !important; opacity: .45 !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-image { opacity: 1 !important; }
[data-dsh-fairy-mascot-root="true"] .dsh-fairy-glitch-blocks { display: none !important; }
${assets.CSS}
</style>
</head>
<body>
<div class="dsh-hdd-background-host" aria-hidden="true">
  <div class="dsh-hdd-fx">
    <div class="dsh-hdd-glow dsh-hdd-glow-a"></div>
    <div class="dsh-hdd-glow dsh-hdd-glow-b"></div>
    <div class="dsh-hdd-glow dsh-hdd-glow-c"></div>
  </div>
</div>
<div class="dsh-fairy-stage" data-visible="true">
  <div id="dsh-fairy-root" data-dsh-fairy-mascot-root="true" data-state="normal" aria-hidden="true">
    <div class="dsh-fairy-float">
${assets.HALO_SVG}
${assets.PULSE_SVG}
${assets.SVG}
    </div>
  </div>
</div>
</body>
</html>
`;

const size = process.argv[4] || '454';
fs.writeFileSync(out, html.replace(/MIN_SIZE/g, size), 'utf8');
console.log(`wrote ${out} (${html.length} bytes, geometry=${JSON.stringify(assets.geometry && {
  outerDiscRadius: assets.geometry.outerDiscRadius,
})})`);
