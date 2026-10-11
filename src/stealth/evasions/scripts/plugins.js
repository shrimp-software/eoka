
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
    pluginArray.item = _eoka_mark((i) => pluginArray[i] || null, 'item');
    pluginArray.namedItem = _eoka_mark((name) => pluginArray[name] || null, 'namedItem');
    pluginArray.refresh = _eoka_mark(() => {}, 'refresh');
    return pluginArray;
};

const _eoka_pluginArray = _eoka_buildPlugins();
Object.defineProperty(Navigator.prototype, 'plugins', {
    get: _eoka_mark(() => _eoka_pluginArray, 'get plugins'),
    configurable: true,
    enumerable: true
});
