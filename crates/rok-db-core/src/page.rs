/// One page of results, returned by [`Select::paginate`](crate::Select::paginate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// Records on this page.
    pub items: Vec<T>,
    /// Total number of matching records across all pages.
    pub total: u64,
    /// Current page number (1-based).
    pub page: u64,
    /// Maximum number of records per page.
    pub per_page: u64,
}

impl<T> Page<T> {
    /// Assemble a page.
    pub fn new(items: Vec<T>, total: u64, page: u64, per_page: u64) -> Self {
        Self {
            items,
            total,
            page,
            per_page,
        }
    }

    /// Total number of pages (at least 1).
    pub fn total_pages(&self) -> u64 {
        if self.per_page == 0 {
            return 1;
        }
        self.total.div_ceil(self.per_page).max(1)
    }

    /// `true` if a later page exists.
    pub fn has_next(&self) -> bool {
        self.page < self.total_pages()
    }

    /// `true` if an earlier page exists.
    pub fn has_prev(&self) -> bool {
        self.page > 1
    }

    /// `true` if this page holds no records.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of records on this page.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Iterate over the records on this page.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    /// Transform every record, keeping pagination metadata (e.g. into DTOs).
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            total: self.total,
            page: self.page,
            per_page: self.per_page,
        }
    }
}

impl<T> IntoIterator for Page<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Page<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_math() {
        let p = Page::new(vec![1, 2], 5, 1, 2);
        assert_eq!(p.total_pages(), 3);
        assert!(p.has_next() && !p.has_prev());
        let p = Page::new(Vec::<i32>::new(), 0, 1, 10);
        assert_eq!(p.total_pages(), 1);
        assert!(!p.has_next());
        assert_eq!(Page::new(vec![1], 1, 1, 1).map(|x| x * 2).items, vec![2]);
    }
}
