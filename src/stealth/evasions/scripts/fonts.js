
// `FontFaceSet.prototype.check` resolves installed system fonts, so detectors
// enumerate font names through it. Families the identity claims (plus CSS
// generic families) resolve; other system names report missing. Families the
// page registered itself (@font-face or `document.fonts.add`) fall through to
// the native check, which reports their real load state.
if (typeof FontFaceSet !== 'undefined' && FontFaceSet.prototype.check) {
    const claimedFonts = new Set(__EOKA_FONTS__.map(f => f.toLowerCase()));
    const genericFonts = new Set(__EOKA_GENERIC_FONTS__);
    const unquote = (family) => family.trim().replace(/^['"]|['"]$/g, '');
    const familyAllowed = (family) =>
        claimedFonts.has(family.toLowerCase()) || genericFonts.has(family.toLowerCase());

    _eoka_patchMethod(FontFaceSet.prototype, 'check', (original) => function(...args) {
        const font = args[0];
        if (typeof font !== 'string' || font === '') return original.apply(this, args);
        const sizeMatch = font.match(/(?:\d+(?:\.\d+)?(?:px|pt|em|rem|ex|ch|vh|vw|%)|smaller|larger|(?:x{1,2}-)?small|medium|(?:x{1,2}-)?large)\s*(.*)$/i);
        const familyList = sizeMatch ? sizeMatch[1] : font;
        if (!familyList.trim()) return original.apply(this, args);
        const families = familyList.split(',').map(unquote);
        if (!families.length || families.some(f => !f)) return original.apply(this, args);
        // Run the native check first so invalid shorthand still throws.
        original.apply(this, args);
        const pageFamilies = new Set();
        for (const face of this) pageFamilies.add(unquote(face.family).toLowerCase());
        const prefix = font.slice(0, font.length - familyList.length);
        return families.every(family => pageFamilies.has(family.toLowerCase())
            ? original.call(this, prefix + JSON.stringify(family), ...args.slice(1))
            : familyAllowed(family));
    });
}
