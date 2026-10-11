
const _eoka_webdriverGet = _eoka_mark(() => false, 'get webdriver');
Object.defineProperty(Object.getPrototypeOf(navigator), 'webdriver', {
    get: _eoka_webdriverGet,
    configurable: true,
    enumerable: true
});
try {
    Object.defineProperty(Navigator.prototype, 'webdriver', {
        get: _eoka_webdriverGet,
        configurable: true,
        enumerable: true
    });
} catch(e) {}

// Remove automation-related properties from window
const automationProps = [
    'callPhantom', '_phantom', 'phantom', '__nightmare', 'domAutomation',
    'domAutomationController', '_selenium', '_Selenium_IDE_Recorder',
    'callSelenium', '__webdriver_script_fn', '__driver_evaluate',
    '__webdriver_evaluate', '__fxdriver_evaluate', '__driver_unwrapped',
    '__webdriver_unwrapped', '__fxdriver_unwrapped', '__selenium_unwrapped',
    '_WEBDRIVER_ELEM_CACHE', 'ChromeDriverw', 'driver-evaluate',
    'webdriver-evaluate', 'selenium-evaluate', 'webdriverCommand',
    'webdriver-evaluate-response', '__webdriverFunc', '__lastWatirAlert',
    '__lastWatirConfirm', '__lastWatirPrompt', '$chrome_asyncScriptInfo',
    '__selenium_evaluate', '__webdriver_script_function'
];

automationProps.forEach(prop => {
    try {
        if (prop in window) delete window[prop];
    } catch(e) {}
});

// Fix for Chrome's runtime.bindingsCalled check
if (window.chrome && window.chrome.runtime) {
    try {
        delete window.chrome.runtime.bindingsCalled;
    } catch(e) {}
}
