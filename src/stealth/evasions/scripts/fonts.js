
if (document.fonts && document.fonts.check) {
    const origCheck = document.fonts.check.bind(document.fonts);
    const claimedFonts = new Set(__EOKA_FONTS__.map(f => f.toLowerCase()));
    const genericFonts = new Set(__EOKA_GENERIC_FONTS__);
    const familyAllowed = (family) =>
        claimedFonts.has(family.toLowerCase()) || genericFonts.has(family.toLowerCase());

    document.fonts.check = _eoka_mark(function check(font, text) {
        if (typeof font !== 'string' || font === '') return origCheck(font, text);
        const sizeMatch = font.match(/(?:\d+(?:\.\d+)?(?:px|pt|em|rem|ex|ch|vh|vw|%)|[xx]-?small|medium|[xx]-?large)\s*(.*)$/i);
        const familyList = sizeMatch ? sizeMatch[1] : font;
        if (!familyList.trim()) return origCheck(font, text);
        const families = familyList.split(',').map(f => f.trim().replace(/^['"]|['"]$/g, ''));
        if (!families.length || families.some(f => !f)) return origCheck(font, text);
        return families.every(familyAllowed);
    }, 'check');
}
