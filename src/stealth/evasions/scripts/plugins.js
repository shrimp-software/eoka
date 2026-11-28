
// Stock Chrome (headless/new included) ships the five PDF viewer plugins
// natively; only an empty list is filled.
const _eoka_buildPlugins = () => {
    const plugins = [
        { name: 'PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chrome PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chromium PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Microsoft Edge PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'WebKit built-in PDF', filename: 'internal-pdf-viewer', description: 'Portable Document Format' }
    ];

    const pluginArray = Object.create(PluginArray.prototype);
    plugins.forEach((p, i) => {
        const plugin = Object.create(Plugin.prototype);
        Object.defineProperties(plugin, {
            name: { value: p.name, enumerable: true },
            filename: { value: p.filename, enumerable: true },
            description: { value: p.description, enumerable: true },
            length: { value: 1, enumerable: true }
        });
        pluginArray[i] = plugin;
        pluginArray[p.name] = plugin;
    });

    Object.defineProperty(pluginArray, 'length', { value: plugins.length });
    const methods = {
        item(i) { return pluginArray[i] || null; },
        namedItem(name) { return pluginArray[name] || null; },
        refresh() {}
    };
    for (const [name, method] of Object.entries(methods)) pluginArray[name] = _eoka_mark(method);
    return pluginArray;
};

if (navigator.plugins.length === 0) {
    const _eoka_pluginArray = _eoka_buildPlugins();
    _eoka_patchGetter(Navigator.prototype, 'plugins', () => function() { return _eoka_pluginArray; });
}
