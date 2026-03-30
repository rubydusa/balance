grammar Balance;

// --- Top-level ---

program
  : moduleDecl? importDecl* (declaration | statement)* EOF
  ;

moduleDecl
  : 'module' qualifiedName
  ;

importDecl
  : 'import' qualifiedName ('.' '{' IDENT (',' IDENT)* '}')?
  | 'import' qualifiedName 'as' IDENT
  ;

qualifiedName
  : IDENT ('.' IDENT)*
  ;

// --- Declarations ---

declaration
  : 'export'? portDecl
  | 'export'? serviceDecl
  | 'export'? typeDecl
  | entryDecl
  | 'pure'? fnDecl
  | substrateDecl
  | guaranteeDecl
  | profileDecl
  | macroDecl
  ;

portDecl
  : 'port' IDENT '{' portMethod* '}'
  ;

portMethod
  : IDENT '(' paramList? ')' '->' typeExpr annotations?
  ;

annotations
  : '[' annotation (',' annotation)* ']'
  ;

annotation
  : IDENT ('(' IDENT ')')?
  ;

serviceDecl
  : 'service' IDENT ('provides' IDENT)? '{' serviceItem* '}'
  ;

serviceItem
  : publishDecl
  | onClause
  | replicatedDecl
  | commandImpl
  | queryImpl
  | componentDecl
  ;

publishDecl
  : 'publish' 'as' STRING
  ;

onClause
  : 'on' expression whereClause? block?
  ;

whereClause
  : 'where' expression
  ;

replicatedDecl
  : 'on' 'replicated' '(' INT ')'
  ;

commandImpl
  : 'command' IDENT '(' paramList? ')' '->' typeExpr
    (block
    | 'via' expression
      'settle' eventRef 'by' expression)
  ;

queryImpl
  : 'query' IDENT '(' paramList? ')' '->' typeExpr
    (block
    | 'via' expression
      'observe' eventRef 'by' expression
      'return' expression)
  ;

eventRef
  : IDENT '.' IDENT
  ;

componentDecl
  : 'component' IDENT '=' 'spawn' IDENT '(' argList? ')'
  ;

entryDecl
  : 'entry' IDENT? '(' paramList? ')' block
  ;

typeDecl
  : 'type' IDENT typeParams? '{' field* '}'
  ;

typeParams
  : '<' IDENT (',' IDENT)* '>'
  ;

field
  : IDENT ':' typeExpr
  ;

fnDecl
  : 'fn' IDENT '(' paramList? ')' ('->' typeExpr)? block
  ;

substrateDecl
  : 'substrate' IDENT typeParams? '{' substrateBody* '}'
  ;

substrateBody
  : 'op' IDENT '(' paramList? ')' '->' typeExpr block?     // op with optional body
  | 'emits' '{' emitEvent* '}'                             // event declarations
  | 'state' '{' field* '}'                                 // state block
  | 'uses' IDENT ':' IDENT                                 // composition dependency
  | fnDecl                                                  // substrate-local functions
  | onClause                                                // substrate on-clauses
  ;

emitEvent
  : IDENT ('{' IDENT (',' IDENT)* '}')?
  ;

guaranteeDecl
  : 'guarantee' IDENT '{' guaranteeBody* '}'
  ;

guaranteeBody
  : 'law' ':' .*?
  ;

profileDecl
  : 'profile' IDENT '{' profilePref* '}'
  ;

profilePref
  : IDENT ':' STRING
  ;

macroDecl
  : 'macro' IDENT '(' macroParamList? ')' block
  ;

macroParamList
  : macroParam (',' macroParam)*
  ;

macroParam
  : IDENT ':' ('Expr' | 'Stmt' | 'Ident' | 'Block')
  ;

// --- Parameters & Types ---

paramList
  : param (',' param)*
  ;

param
  : IDENT ':' typeExpr
  ;

typeExpr
  : 'cap' IDENT authorityQualifier?                  // capability type with optional authority
  | IDENT ('<' typeExpr (',' typeExpr)* '>')? '?'?    // named type with optional type args and nullable
  ;

authorityQualifier
  : '@' ('consume' | 'borrow' | 'delegate')
  ;

// --- Statements ---

statement
  : letStmt
  | assignStmt
  | returnStmt
  | ifStmt
  | matchStmt
  | forStmt
  | whileStmt
  | emitStmt
  | 'break'
  | 'continue'
  | expression
  ;

letStmt
  : 'let' 'mut'? IDENT (':' typeExpr)? '=' expression
  ;

assignStmt
  : IDENT '=' expression
  ;

returnStmt
  : 'return' expression?
  ;

ifStmt
  : 'if' expression block ('else' (ifStmt | block))?
  ;

matchStmt
  : 'match' expression '{' matchArm* '}'
  ;

forStmt
  : 'for' IDENT 'in' expression block
  ;

whileStmt
  : 'while' expression block
  ;

emitStmt
  : 'emit' IDENT '{' emitField* '}'
  ;

emitField
  : IDENT ':' expression
  ;

matchArm
  : pattern guardClause? '=>' (block | statement) ','?
  ;

guardClause
  : 'if' expression
  ;

pattern
  : 'Ok' '(' pattern ')'                             // Result Ok destructuring
  | 'Err' '(' pattern ')'                            // Result Err destructuring
  | 'Some' '(' pattern ')'                           // Option Some destructuring
  | 'None'                                            // Option None pattern
  | IDENT '{' structPatternField (',' structPatternField)* '}'  // struct destructuring
  | '[' listPatternElements? ']'                      // list destructuring
  | literal
  | IDENT                                             // variable binding or wildcard '_'
  ;

structPatternField
  : IDENT (':' pattern)?
  ;

listPatternElements
  : pattern (',' pattern)* (',' '..' pattern)?
  | '..' pattern
  ;

block
  : '{' statement* '}'
  ;

// --- Expressions ---
//
// Precedence levels (low to high):
//  1:  ||          logical or
//  2:  &&          logical and
//  3:  == !=       equality
//  4:  < > <= >=   comparison
//  5:  |           bitwise or
//  6:  ^           bitwise xor
//  7:  &           bitwise and
//  8:  << >>       bitwise shift
//  9:  + -         addition, subtraction
// 10:  * / %       multiplication, division, modulo
// 11:  unary       - ! ~
// Postfix:         .method() .field [index] (call) ?

expression
  : unaryOp expression                                // unary (prec 11)
  | expression '%' expression                         // modulo (prec 10)
  | expression mulDivOp expression                    // mul/div (prec 10)
  | expression addSubOp expression                    // add/sub (prec 9)
  | expression shiftOp expression                     // bitwise shift (prec 8)
  | expression '&' expression                         // bitwise and (prec 7)
  | expression '^' expression                         // bitwise xor (prec 6)
  | expression '|' expression                         // bitwise or (prec 5)
  | expression compOp expression                      // comparison (prec 4)
  | expression eqOp expression                        // equality (prec 3)
  | expression '&&' expression                        // logical and (prec 2)
  | expression '||' expression                        // logical or (prec 1)
  | expression '.' IDENT '(' argList? ')'             // method call
  | expression '.' IDENT                              // field access
  | expression '(' argList? ')'                       // function call
  | expression '[' expression ']'                     // index access
  | expression '?'                                    // try operator (postfix)
  | 'await' expression                                // await interaction
  | 'match' expression '{' matchArm* '}'              // match as expression
  | selectExpr
  | resolveExpr
  | structLiteral
  | listLiteral
  | mapLiteral
  | closureExpr
  | concurrentExpr
  | macroCallExpr
  | importExpr
  | primary
  ;

selectExpr
  : 'select' expression? '{' selectArm+ ('else' '=>' block)? '}'
  ;

selectArm
  : IDENT '=' expression '=>' block
  ;

resolveExpr
  : 'resolve' IDENT '[' expression ']' ('with' 'profile' IDENT)?
  ;

structLiteral
  : IDENT '{' structField (',' structField)* '}'
  ;

structField
  : IDENT ':' expression
  ;

listLiteral
  : '[' (expression (',' expression)* ','?)? ']'
  ;

mapLiteral
  : '{' STRING ':' expression (',' STRING ':' expression)* ','? '}'
  ;

closureExpr
  : '|' closureParamList? '|' (block | expression)
  | '||' (block | expression)
  ;

closureParamList
  : closureParam (',' closureParam)*
  ;

closureParam
  : IDENT (':' typeExpr)?
  ;

concurrentExpr
  : 'concurrent' '{' (expression ';'?)* '}'
  ;

macroCallExpr
  : IDENT '!' '(' argList? ')'
  ;

importExpr
  : 'import' '(' expression ')'
  ;

primary
  : literal
  | 'none'
  | 'ok' '(' expression ')'                          // Result Ok constructor
  | 'err' '(' expression ')'                         // Result Err constructor
  | IDENT
  | '(' expression ')'
  | block
  ;

argList
  : expression (',' expression)*
  ;

literal
  : STRING
  | INT
  | HEXINT
  | FLOAT
  | BYTES
  | 'true'
  | 'false'
  ;

unaryOp : '-' | '!' | '~' ;
mulDivOp : '*' | '/' ;
addSubOp : '+' | '-' ;
shiftOp : '<<' | '>>' ;
compOp : '<' | '>' | '<=' | '>=' ;
eqOp : '==' | '!=' ;

// --- Tokens ---

IDENT  : [a-zA-Z_][a-zA-Z0-9_]* ;
STRING : '"' (~["\\\r\n] | '\\' .)* '"' ;
INT    : [0-9]+ ;
HEXINT : '0x' [0-9a-fA-F]+ ;
FLOAT  : [0-9]+ '.' [0-9]+ ;
BYTES  : 'b"' [0-9a-fA-F]* '"' ;

LINE_COMMENT  : '//' ~[\r\n]* -> skip ;
BLOCK_COMMENT : '/*' .*? '*/' -> skip ;
WS            : [ \t\r\n]+ -> skip ;
