
// Stock Chrome (headless/new included) ships the five PDF viewer plugins
// natively; only an empty list is filled.
const _eoka_buildPlugins = () => {
    const names = ['PDF Viewer', 'Chrome PDF Viewer', 'Chromium PDF Viewer',
        'Microsoft Edge PDF Viewer', 'WebKit built-in PDF'];

    const pluginArray = Object.create(PluginArray.prototype);
    names.forEach((name, i) => {
        const plugin = _eoka_instance(Plugin.prototype, {
            name,
            filename: 'internal-pdf-viewer',
            description: 'Portable Document Format',
            length: 1
        });
        pluginArray[i] = plugin;
        pluginArray[name] = plugin;
    });

    Object.defineProperty(pluginArray, 'length', { value: names.length });
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
