// Simple CRUD API Server
// This is a minimal Express.js server demonstrating basic CRUD operations
// In a real application, this would include:
// - Database connection pooling
// - Request validation and sanitization
// - Error handling middleware
// - Authentication and authorization
// - Rate limiting and security headers

require('dotenv').config();
const express = require('express');
const app = express();

const PORT = process.env.PORT || 3000;

app.use(express.json());

// In-memory storage (replace with actual database in production)
let items = [];
let nextId = 1;

// CREATE - Add a new item
app.post('/api/items', (req, res) => {
  const item = {
    id: nextId++,
    ...req.body,
    createdAt: new Date().toISOString()
  };
  items.push(item);
  res.status(201).json(item);
});

// READ - Get all items
app.get('/api/items', (req, res) => {
  res.json(items);
});

// READ - Get a specific item
app.get('/api/items/:id', (req, res) => {
  const item = items.find(i => i.id === parseInt(req.params.id));
  if (!item) {
    return res.status(404).json({ error: 'Item not found' });
  }
  res.json(item);
});

// UPDATE - Update an item
app.put('/api/items/:id', (req, res) => {
  const index = items.findIndex(i => i.id === parseInt(req.params.id));
  if (index === -1) {
    return res.status(404).json({ error: 'Item not found' });
  }
  items[index] = {
    ...items[index],
    ...req.body,
    updatedAt: new Date().toISOString()
  };
  res.json(items[index]);
});

// DELETE - Delete an item
app.delete('/api/items/:id', (req, res) => {
  const index = items.findIndex(i => i.id === parseInt(req.params.id));
  if (index === -1) {
    return res.status(404).json({ error: 'Item not found' });
  }
  items.splice(index, 1);
  res.status(204).send();
});

// Health check endpoint
app.get('/health', (req, res) => {
  res.json({ status: 'healthy', timestamp: new Date().toISOString() });
});

app.listen(PORT, () => {
  console.log(`CRUD API server running on port ${PORT}`);
  console.log(`Environment: ${process.env.NODE_ENV || 'development'}`);
});
