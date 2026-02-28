use std::collections::BTreeSet;

/// Quarterly contract definition.
#[derive(Debug, Clone)]
pub struct ContractSpec {
    pub symbol: String,
    pub instrument_id: u32,
    pub start_date: i32,
    pub end_date: i32,
    pub rollover_date: i32,
}

/// Manages contract transitions and excluded dates around rollovers.
#[derive(Debug, Clone, Default)]
pub struct RolloverCalendar {
    contracts: Vec<ContractSpec>,
}

impl RolloverCalendar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_contract(&mut self, spec: ContractSpec) {
        self.contracts.push(spec);
    }

    pub fn contracts(&self) -> &[ContractSpec] {
        &self.contracts
    }

    /// Excluded dates: rollover date + 3 days before each rollover.
    pub fn is_excluded(&self, date: i32) -> bool {
        for c in &self.contracts {
            if date == c.rollover_date {
                return true;
            }
            if date >= c.rollover_date - 3 && date < c.rollover_date {
                return true;
            }
        }
        false
    }

    pub fn get_contract_for_date(&self, date: i32) -> Option<&ContractSpec> {
        self.contracts
            .iter()
            .find(|c| date >= c.start_date && date <= c.end_date)
    }

    pub fn excluded_dates(&self) -> BTreeSet<i32> {
        let mut dates = BTreeSet::new();
        for c in &self.contracts {
            dates.insert(c.rollover_date);
            dates.insert(c.rollover_date - 1);
            dates.insert(c.rollover_date - 2);
            dates.insert(c.rollover_date - 3);
        }
        dates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_excluded() {
        let mut cal = RolloverCalendar::new();
        cal.add_contract(ContractSpec {
            symbol: "MESH2".to_string(),
            instrument_id: 1,
            start_date: 20220103,
            end_date: 20220318,
            rollover_date: 20220318,
        });
        assert!(cal.is_excluded(20220318)); // rollover day
        assert!(cal.is_excluded(20220317)); // -1
        assert!(cal.is_excluded(20220316)); // -2
        assert!(cal.is_excluded(20220315)); // -3
        assert!(!cal.is_excluded(20220314)); // -4, not excluded
    }

    #[test]
    fn test_get_contract_for_date() {
        let mut cal = RolloverCalendar::new();
        cal.add_contract(ContractSpec {
            symbol: "MESH2".to_string(),
            instrument_id: 1,
            start_date: 20220103,
            end_date: 20220318,
            rollover_date: 20220318,
        });
        assert!(cal.get_contract_for_date(20220110).is_some());
        assert!(cal.get_contract_for_date(20220401).is_none());
    }
}
