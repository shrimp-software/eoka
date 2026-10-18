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
// without its internal slots makes EventTarget methods throw), so mark every
// function property native. A descriptor/toString probe then sees ordinary
// native handlers instead of arrow-function source.
const _eoka_status = (name, state) => {
    const status = {
        state: state,
        name: name,
        onchange: null,
        addEventListener: _eoka_mark(function addEventListener() {}, 'addEventListener'),
        removeEventListener: _eoka_mark(function removeEventListener() {}, 'removeEventListener'),
        dispatchEvent: _eoka_mark(function dispatchEvent() { return true; }, 'dispatchEvent')
    };
    return status;
};

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
            addEventListener: result.addEventListener?.bind(result) || _eoka_mark(function addEventListener() {}, 'addEventListener'),
            removeEventListener: result.removeEventListener?.bind(result) || _eoka_mark(function removeEventListener() {}, 'removeEventListener'),
            dispatchEvent: result.dispatchEvent?.bind(result) || _eoka_mark(function dispatchEvent() { return true; }, 'dispatchEvent')
        };
    }).catch(function() {
        return _eoka_status(name, 'prompt');
    });
}, 'query');
