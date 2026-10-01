const unit = n => typeof n === 'number' && Number.isFinite(n) && n >= 0 && n <= 1;
export function forecastResult(value) {
  const outcomes = value?.outcomes;
  if (typeof value?.shouldSend !== 'boolean' || !outcomes ||
      !['reply', 'silence', 'negative'].every(k => unit(outcomes[k])) ||
      Math.abs(outcomes.reply + outcomes.silence + outcomes.negative - 1) > 0.02 ||
      !['answer', 'ask', 'acknowledge', 'wait'].includes(value.responseMode) ||
      (value.responseMode === 'wait' && value.shouldSend) ||
      typeof value.plan !== 'string' || !value.plan.trim() || value.plan.length > 400) {
    throw Object.assign(Error('Invalid sending forecast'), { code: 'invalid_forecast' });
  }
  return { shouldSend: value.shouldSend, outcomes: Object.fromEntries(['reply', 'silence', 'negative'].map(k => [k, outcomes[k]])), responseMode: value.responseMode, plan: value.plan.trim() };
}

export function sendingProbability(settings, { proactive, age, gap, recentHumans, score }, forecast) {
  const factors = {
    base: proactive ? settings.proactiveProbability : settings.addressedProbability,
    settle: proactive ? Math.min(1, Math.max(0, age) / settings.settleSeconds) : 1,
    recovery: proactive ? Math.min(1, Math.max(0, gap) / settings.recoverySeconds) : 1,
    pace: proactive ? 1 / (1 + recentHumans / settings.burstScale) : 1,
    motivation: proactive ? 0.25 + 0.75 * (Math.max(1, Math.min(5, score)) - 1) / 4 : 1,
    forecast: proactive ? 1 - forecast.outcomes.negative : 1,
  };
  const veto = !forecast.shouldSend ? 'forecast_withhold' : forecast.outcomes.negative > settings.maxNegativeProbability ? 'forecast_risk' : null;
  return { factors, probability: veto ? 0 : Object.values(factors).reduce((a, b) => a * b, 1), veto };
}
