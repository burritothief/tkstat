use super::usage::ModelFamily;

/// Per-model pricing in USD per 1M tokens.
#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_per_1m: f64,
    pub output_per_1m: f64,
    pub cache_read_per_1m: f64,
    pub cache_creation_per_1m: f64,
}

impl ModelPricing {
    pub fn calculate(&self, input: u64, output: u64, cache_read: u64, cache_creation: u64) -> f64 {
        (input as f64 * self.input_per_1m
            + output as f64 * self.output_per_1m
            + cache_read as f64 * self.cache_read_per_1m
            + cache_creation as f64 * self.cache_creation_per_1m)
            / 1_000_000.0
    }
}

static OPUS_PRICING: ModelPricing = ModelPricing {
    input_per_1m: 15.0,
    output_per_1m: 75.0,
    cache_read_per_1m: 1.50,
    cache_creation_per_1m: 18.75,
};

static SONNET_PRICING: ModelPricing = ModelPricing {
    input_per_1m: 3.0,
    output_per_1m: 15.0,
    cache_read_per_1m: 0.30,
    cache_creation_per_1m: 3.75,
};

static HAIKU_PRICING: ModelPricing = ModelPricing {
    input_per_1m: 0.80,
    output_per_1m: 4.0,
    cache_read_per_1m: 0.08,
    cache_creation_per_1m: 1.0,
};

/// Look up pricing for a model family. Unknown models fall back to Sonnet rates.
pub fn pricing_for(family: ModelFamily) -> &'static ModelPricing {
    match family {
        ModelFamily::Opus => &OPUS_PRICING,
        ModelFamily::Sonnet => &SONNET_PRICING,
        ModelFamily::Haiku => &HAIKU_PRICING,
        ModelFamily::Unknown => &SONNET_PRICING,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_for_known_models() {
        let opus = pricing_for(ModelFamily::Opus);
        assert!((opus.input_per_1m - 15.0).abs() < 0.001);

        let sonnet = pricing_for(ModelFamily::Sonnet);
        assert!((sonnet.input_per_1m - 3.0).abs() < 0.001);
    }

    #[test]
    fn test_unknown_falls_back_to_sonnet() {
        let unknown = pricing_for(ModelFamily::Unknown);
        let sonnet = pricing_for(ModelFamily::Sonnet);
        assert!((unknown.input_per_1m - sonnet.input_per_1m).abs() < 0.001);
    }

    #[test]
    fn test_calculate_cost() {
        let pricing = pricing_for(ModelFamily::Opus);
        let cost = pricing.calculate(1_000_000, 0, 0, 0);
        assert!((cost - 15.0).abs() < 0.001);
    }
}
