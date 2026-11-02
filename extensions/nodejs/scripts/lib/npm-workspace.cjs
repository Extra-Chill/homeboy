// Resolve only declared npm workspace members; never borrow an unrelated lockfile.
const fs = require('fs');
const path = require('path');

function matches(pattern, member) {
    pattern = pattern.replace(/^\.\//, '').replace(/\/$/, '');
    // Expand brace alternatives before translating path-aware glob wildcards.
    const brace = pattern.match(/\{([^{}]+)\}/);
    if (brace) return brace[1].split(',').some(part => matches(pattern.replace(brace[0], part), member));
    let regex = '';
    for (let i = 0; i < pattern.length; i++) {
        const c = pattern[i];
        if (c === '*' && pattern[i + 1] === '*') {
            i++;
            if (pattern[i + 1] === '/') { regex += '(?:.*/)?'; i++; }
            else regex += '.*';
        } else if (c === '*') regex += '[^/]*';
        else if (c === '?') regex += '[^/]';
        else regex += c.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&');
    }
    return new RegExp(`^${regex}$`).test(member);
}

function workspaceRoot(member, lockfiles) {
    member = fs.realpathSync(member);
    for (let root = path.dirname(member); ; root = path.dirname(root)) {
        try {
            const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
            const patterns = Array.isArray(pkg.workspaces) ? pkg.workspaces : pkg.workspaces?.packages;
            if (Array.isArray(patterns) && patterns.some(p => typeof p === 'string' && matches(p, path.relative(root, member).split(path.sep).join('/')))
                && lockfiles.some(file => fs.existsSync(path.join(root, file)))) return root;
        } catch { /* An absent or invalid manifest cannot declare ownership. */ }
        if (root === path.dirname(root)) return null;
    }
}

module.exports = { workspaceRoot };
