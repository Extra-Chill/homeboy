import AppKit

/// Column width configuration for NativeDataTable
enum DataTableColumnWidth {
    case fixed(CGFloat)
    case flexible(min: CGFloat, max: CGFloat)
    case auto(min: CGFloat, ideal: CGFloat, max: CGFloat)
    
    static var `default`: DataTableColumnWidth {
        .auto(
            min: DataTableConstants.defaultMinColumnWidth,
            ideal: DataTableConstants.defaultIdealColumnWidth,
            max: DataTableConstants.defaultMaxColumnWidth
        )
    }
}

/// Column definition for NativeDataTable
struct DataTableColumn<Item> {
    let id: String
    let title: String
    let width: DataTableColumnWidth
    let alignment: NSTextAlignment
    let sortable: Bool
    let cellProvider: (Item) -> NSView
    let sortComparator: ((Item, Item) -> ComparisonResult)?
    
    init(
        id: String,
        title: String,
        width: DataTableColumnWidth = .default,
        alignment: NSTextAlignment = .left,
        sortable: Bool = true,
        sortComparator: ((Item, Item) -> ComparisonResult)? = nil,
        cellProvider: @escaping (Item) -> NSView
    ) {
        self.id = id
        self.title = title
        self.width = width
        self.alignment = alignment
        self.sortable = sortable
        self.sortComparator = sortComparator
        self.cellProvider = cellProvider
    }
}

// MARK: - Convenience Initializers

extension DataTableColumn {
    
    /// Plain text column
    static func text(
        id: String,
        title: String,
        width: DataTableColumnWidth = .default,
        alignment: NSTextAlignment = .left,
        keyPath: KeyPath<Item, String>,
        nullPlaceholder: String = ""
    ) -> DataTableColumn {
        DataTableColumn(
            id: id,
            title: title,
            width: width,
            alignment: alignment,
            sortComparator: { lhs, rhs in
                lhs[keyPath: keyPath].localizedStandardCompare(rhs[keyPath: keyPath])
            },
            cellProvider: { item in
                let value = item[keyPath: keyPath]
                return makeTextCell(
                    text: value.isEmpty ? nullPlaceholder : value,
                    font: DataTableConstants.defaultFont,
                    color: value.isEmpty ? DataTableConstants.nullTextColor : DataTableConstants.primaryTextColor,
                    alignment: alignment
                )
            }
        )
    }
    
    /// Optional text column
    static func optionalText(
        id: String,
        title: String,
        width: DataTableColumnWidth = .default,
        alignment: NSTextAlignment = .left,
        keyPath: KeyPath<Item, String?>,
        nullPlaceholder: String = "—"
    ) -> DataTableColumn {
        DataTableColumn(
            id: id,
            title: title,
            width: width,
            alignment: alignment,
            sortComparator: { lhs, rhs in
                let lhsVal = lhs[keyPath: keyPath] ?? ""
                let rhsVal = rhs[keyPath: keyPath] ?? ""
                return lhsVal.localizedStandardCompare(rhsVal)
            },
            cellProvider: { item in
                let value = item[keyPath: keyPath]
                return makeTextCell(
                    text: value ?? nullPlaceholder,
                    font: DataTableConstants.defaultFont,
                    color: value == nil ? DataTableConstants.nullTextColor : DataTableConstants.primaryTextColor,
                    alignment: alignment
                )
            }
        )
    }
    
    /// Monospaced text column (for code, permissions, sizes, etc.)
    static func monospaced(
        id: String,
        title: String,
        width: DataTableColumnWidth = .default,
        alignment: NSTextAlignment = .left,
        keyPath: KeyPath<Item, String?>,
        nullPlaceholder: String = "—"
    ) -> DataTableColumn {
        DataTableColumn(
            id: id,
            title: title,
            width: width,
            alignment: alignment,
            sortComparator: { lhs, rhs in
                let lhsVal = lhs[keyPath: keyPath] ?? ""
                let rhsVal = rhs[keyPath: keyPath] ?? ""
                return lhsVal.localizedStandardCompare(rhsVal)
            },
            cellProvider: { item in
                let value = item[keyPath: keyPath]
                return makeTextCell(
                    text: value ?? nullPlaceholder,
                    font: DataTableConstants.monospaceFont,
                    color: value == nil ? DataTableConstants.nullTextColor : DataTableConstants.secondaryTextColor,
                    alignment: alignment
                )
            }
        )
    }
    
    /// Icon with text column (for file names, etc.)
    static func iconWithText(
        id: String,
        title: String,
        width: DataTableColumnWidth = .default,
        textKeyPath: KeyPath<Item, String>,
        iconKeyPath: KeyPath<Item, String>,
        iconColorProvider: ((Item) -> NSColor)? = nil
    ) -> DataTableColumn {
        DataTableColumn(
            id: id,
            title: title,
            width: width,
            alignment: .left,
            sortComparator: { lhs, rhs in
                lhs[keyPath: textKeyPath].localizedStandardCompare(rhs[keyPath: textKeyPath])
            },
            cellProvider: { item in
                let text = item[keyPath: textKeyPath]
                let iconName = item[keyPath: iconKeyPath]
                let iconColor = iconColorProvider?(item) ?? DataTableConstants.secondaryTextColor
                return makeIconTextCell(text: text, iconName: iconName, iconColor: iconColor)
            }
        )
    }
    
    /// Custom column with provided cell view
    static func custom(
        id: String,
        title: String,
        width: DataTableColumnWidth = .default,
        alignment: NSTextAlignment = .left,
        sortable: Bool = false,
        sortComparator: ((Item, Item) -> ComparisonResult)? = nil,
        cellProvider: @escaping (Item) -> NSView
    ) -> DataTableColumn {
        DataTableColumn(
            id: id,
            title: title,
            width: width,
            alignment: alignment,
            sortable: sortable,
            sortComparator: sortComparator,
            cellProvider: cellProvider
        )
    }
}

// MARK: - Cell Factory Helpers

/// Creates a plain text cell
func makeTextCell(
    text: String,
    font: NSFont,
    color: NSColor,
    alignment: NSTextAlignment
) -> NSView {
    let textField = NSTextField(labelWithString: text)
    textField.font = font
    textField.textColor = color
    textField.alignment = alignment
    textField.lineBreakMode = .byTruncatingTail
    textField.cell?.truncatesLastVisibleLine = true
    return textField
}

private func makeIconTextCell(
    text: String,
    iconName: String,
    iconColor: NSColor
) -> NSView {
    let container = NSView()
    container.translatesAutoresizingMaskIntoConstraints = false
    
    let imageView = NSImageView()
    imageView.translatesAutoresizingMaskIntoConstraints = false
    imageView.image = NSImage(systemSymbolName: iconName, accessibilityDescription: nil)
    imageView.contentTintColor = iconColor
    imageView.imageScaling = .scaleProportionallyUpOrDown
    
    let textField = NSTextField(labelWithString: text)
    textField.translatesAutoresizingMaskIntoConstraints = false
    textField.font = DataTableConstants.defaultFont
    textField.textColor = DataTableConstants.primaryTextColor
    textField.lineBreakMode = .byTruncatingTail
    textField.cell?.truncatesLastVisibleLine = true
    
    container.addSubview(imageView)
    container.addSubview(textField)
    
    NSLayoutConstraint.activate([
        imageView.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 2),
        imageView.centerYAnchor.constraint(equalTo: container.centerYAnchor),
        imageView.widthAnchor.constraint(equalToConstant: 16),
        imageView.heightAnchor.constraint(equalToConstant: 16),
        
        textField.leadingAnchor.constraint(equalTo: imageView.trailingAnchor, constant: 6),
        textField.trailingAnchor.constraint(lessThanOrEqualTo: container.trailingAnchor, constant: -2),
        textField.centerYAnchor.constraint(equalTo: container.centerYAnchor)
    ])
    
    return container
}

/// Creates a status cell with icon and colored text
func makeStatusCell(
    text: String,
    iconName: String,
    color: NSColor
) -> NSView {
    let container = NSView()
    container.translatesAutoresizingMaskIntoConstraints = false
    
    let imageView = NSImageView()
    imageView.translatesAutoresizingMaskIntoConstraints = false
    imageView.image = NSImage(systemSymbolName: iconName, accessibilityDescription: nil)
    imageView.contentTintColor = color
    imageView.imageScaling = .scaleProportionallyUpOrDown
    
    let textField = NSTextField(labelWithString: text)
    textField.translatesAutoresizingMaskIntoConstraints = false
    textField.font = DataTableConstants.defaultFont
    textField.textColor = color
    textField.lineBreakMode = .byTruncatingTail
    textField.cell?.truncatesLastVisibleLine = true
    
    container.addSubview(imageView)
    container.addSubview(textField)
    
    NSLayoutConstraint.activate([
        imageView.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 2),
        imageView.centerYAnchor.constraint(equalTo: container.centerYAnchor),
        imageView.widthAnchor.constraint(equalToConstant: 14),
        imageView.heightAnchor.constraint(equalToConstant: 14),
        
        textField.leadingAnchor.constraint(equalTo: imageView.trailingAnchor, constant: 4),
        textField.trailingAnchor.constraint(lessThanOrEqualTo: container.trailingAnchor, constant: -2),
        textField.centerYAnchor.constraint(equalTo: container.centerYAnchor)
    ])
    
    return container
}

/// Creates a loading indicator cell
func makeLoadingCell(text: String = "...") -> NSView {
    let container = NSView()
    container.translatesAutoresizingMaskIntoConstraints = false
    
    let spinner = NSProgressIndicator()
    spinner.translatesAutoresizingMaskIntoConstraints = false
    spinner.style = .spinning
    spinner.controlSize = .small
    spinner.startAnimation(nil)
    
    let textField = NSTextField(labelWithString: text)
    textField.translatesAutoresizingMaskIntoConstraints = false
    textField.font = DataTableConstants.defaultFont
    textField.textColor = NSColor.systemBlue
    textField.lineBreakMode = .byTruncatingTail
    
    container.addSubview(spinner)
    container.addSubview(textField)
    
    NSLayoutConstraint.activate([
        spinner.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 2),
        spinner.centerYAnchor.constraint(equalTo: container.centerYAnchor),
        spinner.widthAnchor.constraint(equalToConstant: 14),
        spinner.heightAnchor.constraint(equalToConstant: 14),
        
        textField.leadingAnchor.constraint(equalTo: spinner.trailingAnchor, constant: 4),
        textField.trailingAnchor.constraint(lessThanOrEqualTo: container.trailingAnchor, constant: -2),
        textField.centerYAnchor.constraint(equalTo: container.centerYAnchor)
    ])
    
    return container
}
