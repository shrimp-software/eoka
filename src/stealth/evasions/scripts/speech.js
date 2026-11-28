
if (typeof speechSynthesis !== 'undefined') {
    const defaultVoices = __EOKA_SPEECH_VOICES__.map(v => _eoka_instance(SpeechSynthesisVoice.prototype, {
        name: v.name,
        lang: v.lang,
        localService: v.localService,
        default: v.default,
        voiceURI: v.voiceURI
    }));

    if (defaultVoices.length > 0) {
        _eoka_patchMethod(SpeechSynthesis.prototype, 'getVoices', (original) => function(...args) {
            const voices = original.apply(this, args);
            return voices.length > 0 ? voices : defaultVoices;
        });
    }
}
