
if (typeof speechSynthesis !== 'undefined') {
    const defaultVoices = __EOKA_SPEECH_VOICES__.map(v => {
        const voice = Object.create(SpeechSynthesisVoice.prototype);
        Object.defineProperties(voice, {
            name: { value: v.name, enumerable: true },
            lang: { value: v.lang, enumerable: true },
            localService: { value: v.localService, enumerable: true },
            default: { value: v.default, enumerable: true },
            voiceURI: { value: v.voiceURI, enumerable: true }
        });
        return voice;
    });

    if (defaultVoices.length > 0) {
        _eoka_patchMethod(SpeechSynthesis.prototype, 'getVoices', (original) => function(...args) {
            const voices = original.apply(this, args);
            return voices.length > 0 ? voices : defaultVoices;
        });
    }
}
