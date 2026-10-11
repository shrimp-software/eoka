
// Fix Notification/Permissions API consistency
const originalPermissionsQuery = navigator.permissions.query.bind(navigator.permissions);

try {
    Object.defineProperty(Notification, 'permission', {
        get: _eoka_mark(() => 'default', 'get permission'),
        configurable: true,
        enumerable: true
    });
} catch(e) {}

navigator.permissions.query = _eoka_mark(function query(parameters) {
    const name = parameters.name;

    if (name === 'notifications') {
        return Promise.resolve({
            state: 'prompt',
            name: 'notifications',
            onchange: null,
            addEventListener: function() {},
            removeEventListener: function() {},
            dispatchEvent: function() { return true; }
        });
    }

    if (name === 'clipboard-read' || name === 'clipboard-write') {
        return Promise.resolve({
            state: 'prompt',
            name: name,
            onchange: null,
            addEventListener: function() {},
            removeEventListener: function() {},
            dispatchEvent: function() { return true; }
        });
    }

    return originalPermissionsQuery(parameters).then(function(result) {
        return {
            state: result.state,
            name: result.name || name,
            onchange: result.onchange,
            addEventListener: result.addEventListener?.bind(result) || function() {},
            removeEventListener: result.removeEventListener?.bind(result) || function() {},
            dispatchEvent: result.dispatchEvent?.bind(result) || function() { return true; }
        };
    }).catch(function() {
        return {
            state: 'prompt',
            name: name,
            onchange: null,
            addEventListener: function() {},
            removeEventListener: function() {},
            dispatchEvent: function() { return true; }
        };
    });
}, 'query');
