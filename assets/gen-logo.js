// Generates the brady-bunch logo: a 3x3 grid of little app windows, each with a
// face glancing toward the centre tile (the host console), like the show's opening grid.
const fs = require('fs');
const path = require('path');
const out = process.argv[2];

const C = {
  bg: '#3b2416', cream: '#f6ead6', ink: '#2a1a10',
  orange: '#e8772e', mustard: '#f2b33d', teal: '#2a9d8f',
  avocado: '#8f9c3a', rust: '#c4512b', sky: '#5fa8c9', pink: '#e58f8a', plum: '#8e5a8b',
};
// darker shade for title bars
const shade = (hex, f = 0.78) => '#' + [1, 3, 5].map(i =>
  Math.round(parseInt(hex.slice(i, i + 2), 16) * f).toString(16).padStart(2, '0')).join('');

const T = 140, GAP = 16, M = 30, R = 20, BAR = 30;
const colors = [C.orange, C.teal, C.mustard, C.avocado, null, C.rust, C.sky, C.pink, C.plum];

function chrome(x, y, fill) {
  const bar = shade(fill);
  return `
  <g transform="translate(${x} ${y})">
    <rect width="${T}" height="${T}" rx="${R}" fill="${fill}"/>
    <path d="M0 ${R}a${R} ${R} 0 0 1 ${R}-${R}h${T - 2 * R}a${R} ${R} 0 0 1 ${R} ${R}v${BAR - R}h-${T}z" fill="${bar}"/>
    <circle cx="20" cy="16" r="5" fill="${C.cream}" opacity=".85"/>
    <circle cx="36" cy="16" r="5" fill="${C.cream}" opacity=".55"/>
    <circle cx="52" cy="16" r="5" fill="${C.cream}" opacity=".35"/>`;
}

function face(col, row) {
  // look toward the centre tile
  const dx = 1 - col, dy = 1 - row, len = Math.hypot(dx, dy) || 1, k = 6;
  const px = (dx / len) * k, py = (dy / len) * k;
  const eye = cx => `
    <circle cx="${cx}" cy="70" r="15" fill="${C.cream}"/>
    <circle cx="${(cx + px).toFixed(1)}" cy="${(70 + py).toFixed(1)}" r="7.5" fill="${C.ink}"/>`;
  return `${eye(48)}${eye(92)}
    <path d="M46 102q24 20 48 0" fill="none" stroke="${C.ink}" stroke-width="8" stroke-linecap="round"/>
  </g>`;
}

function host(x, y) {
  // the centre tile: the host console, with a prompt
  return `${chrome(x, y, C.ink).replace(`fill="${shade(C.ink)}"`, `fill="#120b06"`)}
    <path d="M38 62l24 20-24 20" fill="none" stroke="${C.mustard}" stroke-width="10" stroke-linecap="round" stroke-linejoin="round"/>
    <path d="M72 104h32" stroke="${C.cream}" stroke-width="10" stroke-linecap="round"/>
  </g>`;
}

function grid() {
  let s = '';
  for (let i = 0; i < 9; i++) {
    const col = i % 3, row = Math.floor(i / 3);
    const x = M + col * (T + GAP), y = M + row * (T + GAP);
    s += colors[i] ? chrome(x, y, colors[i]) + face(col, row) : host(x, y);
  }
  return s;
}

const icon = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <title>brady-bunch</title>
  <rect width="512" height="512" rx="56" fill="${C.bg}"/>${grid()}
</svg>
`;

// Wide banner: icon on the left, wordmark on the right.
const font = `font-family="'Segoe UI', 'Helvetica Neue', Arial, sans-serif"`;
const banner = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1280 400" width="1280" height="400">
  <title>brady-bunch</title>
  <rect width="1280" height="400" rx="40" fill="${C.bg}"/>
  <g transform="translate(40 40) scale(0.625)">${grid()}
  </g>
  <text x="420" y="210" textLength="790" lengthAdjust="spacingAndGlyphs" ${font} font-size="132" font-weight="800" letter-spacing="-3" fill="${C.cream}">brady<tspan fill="${C.orange}">-</tspan>bunch</text>
  <text x="426" y="280" textLength="537" lengthAdjust="spacingAndGlyphs" ${font} font-size="38" font-weight="500" fill="${C.mustard}">all your apps, one tiled window</text>
  <g transform="translate(426 312)">
    ${[C.orange, C.mustard, C.teal, C.avocado, C.rust, C.sky, C.pink, C.plum]
      .map((c, i) => `<rect x="${i * 46}" width="38" height="10" rx="5" fill="${c}"/>`).join('')}
  </g>
</svg>
`;

fs.mkdirSync(out, { recursive: true });
fs.writeFileSync(path.join(out, 'logo.svg'), icon);
fs.writeFileSync(path.join(out, 'banner.svg'), banner);
console.log('wrote', out);
