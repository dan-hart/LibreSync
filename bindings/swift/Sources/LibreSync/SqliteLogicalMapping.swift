import Foundation

public enum LibreSyncSqliteLogicalEncoding: String, Codable {
    case plain
    case bool
    case json
    case jsonValue = "json_value"
}

public enum LibreSyncMergePolicy: Codable {
    case lastWriterWins
    case setUnion
    case counter
    case listAppend
    case custom(String)

    enum CodingKeys: String, CodingKey {
        case custom
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .lastWriterWins:
            var container = encoder.singleValueContainer()
            try container.encode("last_writer_wins")
        case .setUnion:
            var container = encoder.singleValueContainer()
            try container.encode("set_union")
        case .counter:
            var container = encoder.singleValueContainer()
            try container.encode("counter")
        case .listAppend:
            var container = encoder.singleValueContainer()
            try container.encode("list_append")
        case .custom(let name):
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(name, forKey: .custom)
        }
    }

    public init(from decoder: Decoder) throws {
        if let single = try? decoder.singleValueContainer(), let value = try? single.decode(String.self) {
            switch value {
            case "last_writer_wins": self = .lastWriterWins
            case "set_union": self = .setUnion
            case "counter": self = .counter
            case "list_append": self = .listAppend
            default: self = .custom(value)
            }
            return
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let name = try container.decode(String.self, forKey: .custom)
        self = .custom(name)
    }
}

public struct LibreSyncSqliteLogicalField: Codable {
    public let column: String
    public let field: String
    public let encoding: LibreSyncSqliteLogicalEncoding?
    public let mergePolicy: LibreSyncMergePolicy?

    public init(
        column: String,
        field: String,
        encoding: LibreSyncSqliteLogicalEncoding? = nil,
        mergePolicy: LibreSyncMergePolicy? = nil
    ) {
        self.column = column
        self.field = field
        self.encoding = encoding
        self.mergePolicy = mergePolicy
    }

    enum CodingKeys: String, CodingKey {
        case column
        case field
        case encoding
        case mergePolicy = "merge_policy"
    }
}

public struct LibreSyncSqliteLogicalMapping: Codable {
    public let dataTable: String
    public let idColumn: String
    public let schema: String
    public let entity: String
    public let fields: [LibreSyncSqliteLogicalField]
    public let metaTable: String?
    public let defaultMergePolicy: LibreSyncMergePolicy?

    public init(
        dataTable: String,
        idColumn: String,
        schema: String,
        entity: String,
        fields: [LibreSyncSqliteLogicalField],
        metaTable: String? = nil,
        defaultMergePolicy: LibreSyncMergePolicy? = nil
    ) {
        self.dataTable = dataTable
        self.idColumn = idColumn
        self.schema = schema
        self.entity = entity
        self.fields = fields
        self.metaTable = metaTable
        self.defaultMergePolicy = defaultMergePolicy
    }

    public func jsonString(pretty: Bool = false) throws -> String {
        let encoder = JSONEncoder()
        if pretty {
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        }
        let data = try encoder.encode(self)
        return String(decoding: data, as: UTF8.self)
    }

    enum CodingKeys: String, CodingKey {
        case dataTable = "data_table"
        case idColumn = "id_column"
        case schema
        case entity
        case fields
        case metaTable = "meta_table"
        case defaultMergePolicy = "default_merge_policy"
    }
}
