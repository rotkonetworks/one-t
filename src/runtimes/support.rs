// The MIT License (MIT)
// Copyright © 2021 Aukbit Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

pub type ChainPrefix = u16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SupportedRuntime {
    Polkadot,
    Kusama,
    Westend,
}

impl SupportedRuntime {
    pub fn chain_prefix(&self) -> ChainPrefix {
        match &self {
            Self::Polkadot => 0,
            Self::Kusama => 2,
            Self::Westend => 42,
        }
    }
}

impl From<String> for SupportedRuntime {
    fn from(v: String) -> Self {
        match v.as_str() {
            "polkadot" => Self::Polkadot,
            "kusama" => Self::Kusama,
            "westend" => Self::Westend,
            _ => unimplemented!("Chain prefix not supported"),
        }
    }
}

impl From<ChainPrefix> for SupportedRuntime {
    fn from(v: ChainPrefix) -> Self {
        match v {
            0 => Self::Polkadot,
            2 => Self::Kusama,
            42 => Self::Westend,
            _ => unimplemented!("Chain prefix not supported"),
        }
    }
}

impl std::fmt::Display for SupportedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Polkadot => write!(f, "Polkadot"),
            Self::Kusama => write!(f, "Kusama"),
            Self::Westend => write!(f, "Westend"),
        }
    }
}
