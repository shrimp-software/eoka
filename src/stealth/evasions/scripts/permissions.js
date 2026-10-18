// Fix Notification/Permissions API consistency
const originalPermissionsQuery = navigator.permissions.query.bind(navigator.permissions);

try {
    Object.defineProperty(Notification, 'permission', {
        get: _eoka_mark(() => 'default', 'get permission'),
        configurable: true,
        enumerable: true
    });
} catch(e) {}

// Status objects are plain records (constructing a real PermissionStatus
// without its internal slots makes EventTarget methods throw). Shorthand
// methods have no `prototype` and cannot be constructed, like native ones, and
// `_eoka_mark` makes `toString()` report native code.
const _eoka_statusMethods = {
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() { return true; }
};
for (const method of Object.values(_eoka_statusMethods)) _eoka_mark(method);

const _eoka_status = (name, state) => ({
    state: state,
    name: name,
    onchange: null,
    ..._eoka_statusMethods
});

navigator.permissions.query = _eoka_mark(function query(parameters) {
    const name = parameters.name;

    if (name === 'notifications') {
        return Promise.resolve(_eoka_status('notifications', 'prompt'));
    }

    if (name === 'clipboard-read' || name === 'clipboard-write') {
        return Promise.resolve(_eoka_status(name, 'prompt'));
    }

    return originalPermissionsQuery(parameters).then(function(result) {
        return {
            state: result.state,
            name: result.name || name,
            onchange: result.onchange,
            addEventListener: result.addEventListener?.bind(result) || _eoka_statusMethods.addEventListener,
            removeEventListener: result.removeEventListener?.bind(result) || _eoka_statusMethods.removeEventListener,
            dispatchEvent: result.dispatchEvent?.bind(result) || _eoka_statusMethods.dispatchEvent
        };
    }).catch(function() {
        return _eoka_status(name, 'prompt');
    });
}, 'query');
