//! The registry of the app's [`Feature`]s.

use crate::app::AppOptions;
use crate::feature::Feature;
use crate::toolbar::Action;

/// Every feature, in the order actions are offered to them.
pub struct Features {
    pub rom: crate::wizard::RomSetup,
    pub capture: crate::capture::Capture,
    pub tools: crate::tools::Tools,
    pub files: crate::files::Files,
    pub help: crate::help::Help,
    /// Features added with [`push`](Self::push) (tests), after the five.
    extra: Vec<Box<dyn Feature>>,
}

impl Features {
    pub fn new(opts: &AppOptions) -> Self {
        Self {
            rom: crate::wizard::RomSetup::new(opts),
            capture: crate::capture::Capture::new(opts),
            tools: crate::tools::Tools::new(opts),
            files: crate::files::Files::new(opts),
            help: crate::help::Help::new(opts),
            extra: Vec::new(),
        }
    }

    /// The five features in field order, then the extra ones.
    pub fn each(&mut self) -> Vec<&mut dyn Feature> {
        let mut all: Vec<&mut dyn Feature> = vec![
            &mut self.rom,
            &mut self.capture,
            &mut self.tools,
            &mut self.files,
            &mut self.help,
        ];
        all.extend(
            self.extra
                .iter_mut()
                .map(|f| f.as_mut() as &mut dyn Feature),
        );
        all
    }

    /// As [`each`](Self::each), shared.
    fn iter(&self) -> Vec<&dyn Feature> {
        let mut all: Vec<&dyn Feature> = vec![
            &self.rom,
            &self.capture,
            &self.tools,
            &self.files,
            &self.help,
        ];
        all.extend(self.extra.iter().map(|f| f.as_ref()));
        all
    }

    /// The earliest deadline any feature reports.
    pub fn next_deadline(&self) -> Option<u64> {
        self.iter()
            .into_iter()
            .filter_map(|f| f.next_deadline())
            .min()
    }

    /// True when any feature blocks calculator input.
    pub fn blocks_input(&self) -> bool {
        self.iter().into_iter().any(|f| f.blocks_input())
    }

    /// True when any feature draws `action` selected.
    pub fn active(&self, action: Action) -> bool {
        self.iter().into_iter().any(|f| f.active(action))
    }

    /// Appends a feature after the five (tests only).
    #[doc(hidden)]
    pub fn push(&mut self, f: Box<dyn Feature>) {
        self.extra.push(f);
    }
}
